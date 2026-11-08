//! Shared server state: the game registry, live sessions and quick-match queues.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use ogap_eventlog::LogStore;
use ogap_game_core::{GameRegistry, GameRng, Seat, SeatCount, Seed};
use ogap_protocol::{ErrorCode, GameId, ObservationMode, ResumeToken, ServerMessage};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::oneshot;

use crate::config::{ServerConfig, ThinkTime};
use crate::session::{
    self, Command, ConnId, Directory, GameHandle, GameSpec, JoinError, Participant, SessionEnv,
};

/// Errors surfaced to bots as `error` messages.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum HubError {
    #[error("unknown game {0:?}")]
    UnknownGame(String),
    #[error("{0}")]
    InvalidConfig(String),
    #[error("no such game session")]
    NoSuchGame,
    #[error("the game is full or has already started")]
    GameFull,
    #[error("invalid resume token")]
    InvalidResume,
    #[error("the server is at capacity")]
    ServerBusy,
}

impl HubError {
    pub(crate) fn to_message(&self) -> ServerMessage {
        let code = match self {
            Self::UnknownGame(_) => ErrorCode::UnknownGame,
            Self::InvalidConfig(_) => ErrorCode::InvalidConfig,
            Self::NoSuchGame => ErrorCode::NoSuchGame,
            Self::GameFull => ErrorCode::GameFull,
            Self::InvalidResume => ErrorCode::InvalidResume,
            Self::ServerBusy => ErrorCode::ServerBusy,
        };
        ServerMessage::error(code, self.to_string(), false)
    }
}

/// Bots are matched only with others asking for the same game, seat count
/// and canonical configuration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct QueueKey {
    game: &'static str,
    seats: SeatCount,
    config: String,
}

/// A request for a game, as sent by a bot, before validation.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct GameRequest<'a> {
    pub(crate) game: &'a str,
    pub(crate) seats: Option<SeatCount>,
    pub(crate) config: Option<&'a Value>,
    pub(crate) think_time_ms: Option<u64>,
    pub(crate) observation_mode: Option<ObservationMode>,
}

/// Snapshot for the status endpoint.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct HubStatus {
    pub(crate) sessions: usize,
    pub(crate) queued: usize,
    pub(crate) connections: usize,
}

/// Decrements the connection count when dropped.
#[derive(Debug)]
pub(crate) struct ConnectionGuard(Arc<Hub>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(crate) struct Hub {
    pub(crate) registry: GameRegistry,
    pub(crate) config: ServerConfig,
    env: SessionEnv,
    queues: Mutex<HashMap<QueueKey, Vec<Participant>>>,
    next_conn: AtomicU64,
    connections: AtomicUsize,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}

impl Hub {
    pub(crate) fn new(
        registry: GameRegistry,
        config: ServerConfig,
        logs: Arc<dyn LogStore>,
    ) -> Self {
        let env = SessionEnv {
            directory: Directory::default(),
            logs,
            reconnect_grace: config.reconnect_grace,
            lobby_timeout: config.lobby_timeout,
            forfeit_policy: config.forfeit_policy,
        };
        Self {
            registry,
            config,
            env,
            queues: Mutex::default(),
            next_conn: AtomicU64::new(1),
            connections: AtomicUsize::new(0),
        }
    }

    pub(crate) fn next_conn_id(&self) -> ConnId {
        self.next_conn.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn try_acquire_connection(self: &Arc<Self>) -> Option<ConnectionGuard> {
        let max = self.config.max_connections;
        self.connections
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < max).then_some(n + 1)
            })
            .ok()
            .map(|_| ConnectionGuard(Arc::clone(self)))
    }

    fn directory(&self) -> std::sync::MutexGuard<'_, HashMap<GameId, GameHandle>> {
        self.env
            .directory
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn handle(&self, id: GameId) -> Option<GameHandle> {
        self.directory().get(&id).cloned()
    }

    pub(crate) fn status(&self) -> HubStatus {
        let queued = self
            .queues
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(Vec::len)
            .sum();
        HubStatus {
            sessions: self.directory().len(),
            queued,
            connections: self.connections.load(Ordering::Relaxed),
        }
    }

    /// Validates a game request into a [`GameSpec`].
    pub(crate) fn spec(&self, req: GameRequest<'_>) -> Result<GameSpec, HubError> {
        let game = self
            .registry
            .get(req.game)
            .ok_or_else(|| HubError::UnknownGame(req.game.to_owned()))?;
        let info = game.info();
        let seats = match req.seats {
            Some(s) => s,
            None => SeatCount::new(info.seats.min)
                .map_err(|e| HubError::InvalidConfig(e.to_string()))?,
        };
        let config = game
            .normalize_config(req.config, seats)
            .map_err(|e| HubError::InvalidConfig(e.to_string()))?;
        let think_time = match req.think_time_ms {
            Some(ms) => {
                ThinkTime::from_millis(ms).map_err(|e| HubError::InvalidConfig(e.to_string()))?
            }
            None => self.config.think_time,
        };
        Ok(GameSpec {
            game: Arc::clone(game),
            seats,
            config,
            think_time,
            observation_mode: req.observation_mode.unwrap_or(self.config.observation_mode),
        })
    }

    fn spawn(&self, spec: GameSpec, players: Vec<Participant>) -> Result<GameId, HubError> {
        if self.directory().len() >= self.config.max_games {
            return Err(HubError::ServerBusy);
        }
        let id = GameId::new();
        session::spawn(id, spec, self.env.clone(), players);
        Ok(id)
    }

    /// Creates an empty lobby.
    pub(crate) fn create_game(&self, spec: GameSpec) -> Result<GameId, HubError> {
        self.spawn(spec, Vec::new())
    }

    /// Takes a seat in a lobby.
    pub(crate) async fn join(
        &self,
        id: GameId,
        player: Participant,
    ) -> Result<GameHandle, HubError> {
        let handle = self.handle(id).ok_or(HubError::NoSuchGame)?;
        let (reply, rx) = oneshot::channel();
        if !handle.send(Command::Join { player, reply }).await {
            return Err(HubError::NoSuchGame);
        }
        match rx.await {
            Ok(Ok(())) => Ok(handle),
            Ok(Err(JoinError::Full)) => Err(HubError::GameFull),
            Err(_) => Err(HubError::NoSuchGame),
        }
    }

    /// Enters the quick-match queue. Sends `queued` to the bot first, then
    /// starts a game if enough compatible bots are waiting. Seats are
    /// assigned in random order.
    pub(crate) fn enqueue(&self, spec: GameSpec, player: Participant) -> Result<(), HubError> {
        let key = QueueKey {
            game: spec.game.info().id,
            seats: spec.seats,
            config: spec.config.to_string(),
        };
        player.outbox.send(ServerMessage::Queued {
            game: key.game.to_owned(),
        });
        let mut queues = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
        let waiting = queues.entry(key.clone()).or_default();
        waiting.push(player);
        if waiting.len() < spec.seats.len() {
            return Ok(());
        }
        let mut table: Vec<Participant> = waiting.drain(..spec.seats.len()).collect();
        if waiting.is_empty() {
            queues.remove(&key);
        }
        GameRng::from_seed(Seed::new(rand::random())).shuffle(&mut table);
        match self.spawn(spec, table.clone()) {
            Ok(_) => Ok(()),
            Err(e) => {
                // Put everyone back so nobody silently loses their place.
                queues.entry(key).or_default().splice(0..0, table);
                Err(e)
            }
        }
    }

    /// Leaves every queue.
    pub(crate) fn dequeue(&self, conn: ConnId) {
        let mut queues = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
        queues.retain(|_, waiting| {
            waiting.retain(|p| p.outbox.conn != conn);
            !waiting.is_empty()
        });
    }

    /// Reclaims a seat.
    pub(crate) async fn reconnect(
        &self,
        id: GameId,
        token: ResumeToken,
        player: Participant,
    ) -> Result<(GameHandle, Seat), HubError> {
        let handle = self.handle(id).ok_or(HubError::NoSuchGame)?;
        let (reply, rx) = oneshot::channel();
        if !handle
            .send(Command::Reconnect {
                token,
                player,
                reply,
            })
            .await
        {
            return Err(HubError::NoSuchGame);
        }
        match rx.await {
            Ok(Ok(seat)) => Ok((handle, seat)),
            Ok(Err(_)) => Err(HubError::InvalidResume),
            Err(_) => Err(HubError::NoSuchGame),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::time::Duration;

    use ogap_connect4::ConnectFour;
    use ogap_eventlog::{EventBody, GameLog, MemoryLogStore, Replayer};
    use ogap_game_core::{ForfeitReason, GameResult};
    use ogap_protocol::{BotInfo, DecisionId, RejectCode, ServerMessage};
    use ogap_tictactoe::TicTacToe;
    use serde_json::json;
    use tokio::sync::mpsc;
    use tokio::time::Instant;

    use super::*;
    use crate::session::Outbox;

    fn registry() -> GameRegistry {
        let mut r = GameRegistry::new();
        r.register(TicTacToe).unwrap();
        r.register(ConnectFour).unwrap();
        r
    }

    fn hub_with(config: ServerConfig) -> (Arc<Hub>, MemoryLogStore) {
        let logs = MemoryLogStore::new();
        let hub = Arc::new(Hub::new(registry(), config, Arc::new(logs.clone())));
        (hub, logs)
    }

    fn hub() -> (Arc<Hub>, MemoryLogStore) {
        hub_with(ServerConfig::default())
    }

    struct TestBot {
        player: Participant,
        rx: mpsc::Receiver<ServerMessage>,
    }

    impl TestBot {
        fn new(hub: &Hub, name: &str) -> Self {
            let (tx, rx) = mpsc::channel(64);
            Self {
                player: Participant {
                    bot: BotInfo::new(name, Some("1.0")).unwrap(),
                    outbox: Outbox::new(hub.next_conn_id(), tx),
                },
                rx,
            }
        }

        async fn recv(&mut self) -> ServerMessage {
            tokio::time::timeout(Duration::from_secs(3_600), self.rx.recv())
                .await
                .expect("timed out waiting for a message")
                .expect("outbox closed")
        }

        /// Receives messages until `pick` matches one.
        async fn expect<T>(&mut self, mut pick: impl FnMut(ServerMessage) -> Option<T>) -> T {
            loop {
                let msg = self.recv().await;
                let dbg = format!("{msg:?}");
                if let Some(v) = pick(msg) {
                    return v;
                }
                assert!(!dbg.contains("GameOver"), "game ended unexpectedly: {dbg}");
            }
        }

        async fn game_start(&mut self) -> (GameId, Seat, ResumeToken) {
            self.expect(|m| match m {
                ServerMessage::GameStart {
                    game_id,
                    seat,
                    resume_token,
                    ..
                } => Some((game_id, seat, resume_token)),
                _ => None,
            })
            .await
        }

        async fn decide(&mut self) -> (DecisionId, u64, Vec<serde_json::Value>) {
            self.expect(|m| match m {
                ServerMessage::Decide {
                    decision_id,
                    remaining_ms,
                    legal_actions,
                    ..
                } => Some((decision_id, remaining_ms, legal_actions.unwrap_or_default())),
                _ => None,
            })
            .await
        }

        async fn game_over(&mut self) -> GameResult {
            loop {
                if let ServerMessage::GameOver { result, .. } = self.recv().await {
                    return result;
                }
            }
        }

        async fn act(
            &self,
            hub: &Hub,
            game: GameId,
            seat: Seat,
            id: DecisionId,
            action: serde_json::Value,
        ) {
            let handle = hub.handle(game).expect("game is live");
            assert!(
                handle
                    .send(Command::Action {
                        from: self.player.outbox.clone(),
                        seat,
                        decision_id: id,
                        action,
                        received: Instant::now(),
                    })
                    .await
            );
        }
    }

    fn ttt_spec(hub: &Hub, think_ms: u64) -> GameSpec {
        hub.spec(GameRequest {
            game: "tictactoe",
            think_time_ms: Some(think_ms),
            ..GameRequest::default()
        })
        .unwrap()
    }

    /// Creates a TTT lobby and seats `a` (seat 0) and `b` (seat 1).
    /// Returns the game id and `a`'s resume token.
    async fn start_ttt_with_token(
        hub: &Hub,
        a: &mut TestBot,
        b: &mut TestBot,
        think_ms: u64,
    ) -> (GameId, ResumeToken) {
        let id = hub.create_game(ttt_spec(hub, think_ms)).unwrap();
        hub.join(id, a.player.clone()).await.unwrap();
        hub.join(id, b.player.clone()).await.unwrap();
        let (ga, sa, token) = a.game_start().await;
        let (gb, sb, _) = b.game_start().await;
        assert_eq!((ga, gb), (id, id));
        assert_eq!((sa, sb), (Seat::FIRST, Seat::SECOND));
        (id, token)
    }

    async fn start_ttt(hub: &Hub, a: &mut TestBot, b: &mut TestBot, think_ms: u64) -> GameId {
        start_ttt_with_token(hub, a, b, think_ms).await.0
    }

    #[tokio::test(start_paused = true)]
    async fn lobby_game_plays_to_completion_and_replays() {
        let (hub, logs) = hub();
        let mut a = TestBot::new(&hub, "alpha");
        let mut b = TestBot::new(&hub, "beta");
        let id = start_ttt(&hub, &mut a, &mut b, 1_000).await;

        // X takes the top row while O plays the middle row.
        for (bot_is_a, cell) in [(true, 0), (false, 3), (true, 1), (false, 4), (true, 2)] {
            let (bot, seat) = if bot_is_a {
                (&mut a, Seat::FIRST)
            } else {
                (&mut b, Seat::SECOND)
            };
            let (decision, remaining, legal) = bot.decide().await;
            assert_eq!(remaining, 1_000);
            assert!(legal.contains(&json!({ "cell": cell })));
            bot.act(&hub, id, seat, decision, json!({ "cell": cell }))
                .await;
            bot.expect(|m| matches!(m, ServerMessage::ActionAccepted { .. }).then_some(()))
                .await;
        }
        let result = a.game_over().await;
        assert_eq!(b.game_over().await, result);
        let GameResult::Completed { ranks, .. } = &result else {
            panic!("expected completion, got {result:?}");
        };
        assert_eq!(ranks.iter().map(|r| r.get()).collect::<Vec<_>>(), [1, 2]);

        // Session cleans up and its log replays exactly.
        tokio::task::yield_now().await;
        assert!(hub.handle(id).is_none());
        let log = GameLog::read(Cursor::new(&logs.logs()[0])).unwrap();
        assert_eq!(log.header().players[0].bot.name.as_str(), "alpha");
        assert_eq!(log.header().think_time_ms, Some(1_000));
        assert_eq!(log.actions().count(), 5);
        Replayer::new(&hub.registry, &log)
            .unwrap()
            .run_to_end()
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn invalid_actions_are_rejected_without_resetting_the_clock() {
        let (hub, logs) = hub();
        let mut a = TestBot::new(&hub, "alpha");
        let mut b = TestBot::new(&hub, "beta");
        let id = start_ttt(&hub, &mut a, &mut b, 1_000).await;
        let (decision, _, _) = a.decide().await;

        tokio::time::advance(Duration::from_millis(600)).await;
        a.act(&hub, id, Seat::FIRST, decision, json!({ "cell": 9 }))
            .await;
        let (code, remaining) = a
            .expect(|m| match m {
                ServerMessage::ActionRejected {
                    code, remaining_ms, ..
                } => Some((code, remaining_ms)),
                _ => None,
            })
            .await;
        assert_eq!(code, RejectCode::MalformedAction);
        assert_eq!(remaining, 400);

        a.act(
            &hub,
            id,
            Seat::FIRST,
            DecisionId::new(99),
            json!({ "cell": 0 }),
        )
        .await;
        let code = a
            .expect(|m| match m {
                ServerMessage::ActionRejected { code, .. } => Some(code),
                _ => None,
            })
            .await;
        assert_eq!(code, RejectCode::StaleDecision);

        // Seat 1 acting out of turn has no pending decision: stale.
        b.act(&hub, id, Seat::SECOND, decision, json!({ "cell": 0 }))
            .await;
        b.expect(|m| {
            matches!(
                m,
                ServerMessage::ActionRejected {
                    code: RejectCode::StaleDecision,
                    ..
                }
            )
            .then_some(())
        })
        .await;

        // A valid retry within the remaining time is accepted.
        tokio::time::advance(Duration::from_millis(300)).await;
        a.act(&hub, id, Seat::FIRST, decision, json!({ "cell": 4 }))
            .await;
        a.expect(|m| matches!(m, ServerMessage::ActionAccepted { .. }).then_some(()))
            .await;

        // Then O occupies the same cell: rules violation.
        let (d2, _, _) = b.decide().await;
        b.act(&hub, id, Seat::SECOND, d2, json!({ "cell": 4 }))
            .await;
        let code = b
            .expect(|m| match m {
                ServerMessage::ActionRejected { code, .. } => Some(code),
                _ => None,
            })
            .await;
        assert_eq!(code, RejectCode::IllegalAction);

        // Let O time out; the log records the rejections, timeout and stats.
        let result = a.game_over().await;
        assert!(matches!(
            result,
            GameResult::Forfeited {
                seat: Seat::SECOND,
                reason: ForfeitReason::Timeout,
                ..
            }
        ));
        tokio::task::yield_now().await;
        let log = GameLog::read(Cursor::new(&logs.logs()[0])).unwrap();
        let rejected = log
            .events()
            .iter()
            .filter(|e| matches!(e.body, EventBody::Rejected { .. }))
            .count();
        assert_eq!(rejected, 2);
        let Some(EventBody::End { stats, .. }) = log.events().last().map(|e| &e.body) else {
            panic!("missing end event");
        };
        assert_eq!(stats[0].actions, 1);
        assert_eq!(stats[0].total_response_ms, 900);
        assert_eq!(stats[0].rejected, 1);
        assert_eq!(stats[1].rejected, 1);
        assert_eq!(stats[1].timeouts, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_expiry_forfeits() {
        let (hub, _) = hub();
        let mut a = TestBot::new(&hub, "alpha");
        let mut b = TestBot::new(&hub, "beta");
        let id = start_ttt(&hub, &mut a, &mut b, 250).await;
        a.decide().await;
        let started = Instant::now();
        let result = b.game_over().await;
        assert_eq!(started.elapsed(), Duration::from_millis(250));
        assert!(matches!(
            result,
            GameResult::Forfeited {
                seat: Seat::FIRST,
                reason: ForfeitReason::Timeout,
                ..
            }
        ));
        assert_eq!(
            result
                .ranks()
                .unwrap()
                .iter()
                .map(|r| r.get())
                .collect::<Vec<_>>(),
            [2, 1]
        );
        tokio::task::yield_now().await;
        assert!(hub.handle(id).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn reconnect_resumes_with_the_running_clock() {
        let (hub, _) = hub();
        let mut a = TestBot::new(&hub, "alpha");
        let mut b = TestBot::new(&hub, "beta");
        let (id, token) = start_ttt_with_token(&hub, &mut a, &mut b, 1_000).await;
        let (decision, _, _) = a.decide().await;

        let wrong = hub
            .reconnect(id, ResumeToken::new("nope"), a.player.clone())
            .await
            .unwrap_err();
        assert_eq!(wrong, HubError::InvalidResume);

        // Seat 0 drops; 300 ms later a new connection reclaims it.
        hub.handle(id)
            .unwrap()
            .send(Command::Disconnected {
                conn: a.player.outbox.conn,
            })
            .await;
        tokio::time::advance(Duration::from_millis(300)).await;

        let mut a2 = TestBot::new(&hub, "alpha");
        let (_, seat) = hub
            .reconnect(id, token.clone(), a2.player.clone())
            .await
            .unwrap();
        assert_eq!(seat, Seat::FIRST);
        let (_, _, same_token) = a2.game_start().await;
        assert_eq!(same_token, token, "a reconnect keeps the seat identity");
        let (d, remaining, _) = a2.decide().await;
        assert_eq!(
            d, decision,
            "the pending decision is re-sent, not re-issued"
        );
        assert_eq!(remaining, 700, "the clock kept running while disconnected");
        a2.act(&hub, id, Seat::FIRST, d, json!({ "cell": 0 })).await;
        a2.expect(|m| matches!(m, ServerMessage::ActionAccepted { .. }).then_some(()))
            .await;

        // The old connection can no longer act for the seat.
        let (d2, _, _) = b.decide().await;
        a.act(&hub, id, Seat::FIRST, d2, json!({ "cell": 1 })).await;
        a.expect(|m| {
            matches!(
                m,
                ServerMessage::Error {
                    code: ErrorCode::Superseded,
                    ..
                }
            )
            .then_some(())
        })
        .await;

        // Reclaiming again from a third connection supersedes the second.
        let mut a3 = TestBot::new(&hub, "alpha");
        hub.reconnect(id, token, a3.player.clone()).await.unwrap();
        a2.expect(|m| {
            matches!(
                m,
                ServerMessage::Error {
                    code: ErrorCode::Superseded,
                    fatal: true,
                    ..
                }
            )
            .then_some(())
        })
        .await;
        a3.game_start().await;
    }

    #[tokio::test(start_paused = true)]
    async fn disconnect_past_grace_forfeits_even_off_turn() {
        let (hub, _) = hub_with(ServerConfig {
            reconnect_grace: Duration::from_millis(200),
            ..ServerConfig::default()
        });
        let mut a = TestBot::new(&hub, "alpha");
        let mut b = TestBot::new(&hub, "beta");
        let id = start_ttt(&hub, &mut a, &mut b, 5_000).await;
        a.decide().await;
        // Seat 1 is not on turn but disconnects and never returns.
        hub.handle(id)
            .unwrap()
            .send(Command::Disconnected {
                conn: b.player.outbox.conn,
            })
            .await;
        let started = Instant::now();
        let result = a.game_over().await;
        assert_eq!(started.elapsed(), Duration::from_millis(200));
        assert!(matches!(
            result,
            GameResult::Forfeited {
                seat: Seat::SECOND,
                reason: ForfeitReason::Disconnected,
                ..
            }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn lobby_full_and_expiry() {
        let (hub, _) = hub_with(ServerConfig {
            lobby_timeout: Duration::from_secs(5),
            ..ServerConfig::default()
        });
        let mut a = TestBot::new(&hub, "alpha");
        let id = hub.create_game(ttt_spec(&hub, 1_000)).unwrap();
        hub.join(id, a.player.clone()).await.unwrap();
        let (filled, total) = a
            .expect(|m| match m {
                ServerMessage::Joined {
                    seats_filled,
                    seats_total,
                    ..
                } => Some((seats_filled, seats_total)),
                _ => None,
            })
            .await;
        assert_eq!((filled, total), (1, 2));
        let result = a.game_over().await;
        assert!(matches!(result, GameResult::Aborted { .. }));
        tokio::task::yield_now().await;
        assert_eq!(
            hub.join(id, a.player.clone()).await.unwrap_err(),
            HubError::NoSuchGame
        );

        // A full lobby rejects a third player.
        let mut b = TestBot::new(&hub, "beta");
        let mut c = TestBot::new(&hub, "gamma");
        let id = start_ttt(&hub, &mut a, &mut b, 1_000).await;
        assert_eq!(
            hub.join(id, c.player.clone()).await.unwrap_err(),
            HubError::GameFull
        );
        assert!(c.rx.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn quick_match_pairs_compatible_bots() {
        let (hub, _) = hub();
        let mut a = TestBot::new(&hub, "alpha");
        let mut b = TestBot::new(&hub, "beta");
        let mut c = TestBot::new(&hub, "gamma");
        let spec = |config| {
            hub.spec(GameRequest {
                game: "connect4",
                config,
                ..GameRequest::default()
            })
            .unwrap()
        };
        let small = json!({"columns": 5, "rows": 5});
        hub.enqueue(spec(None), a.player.clone()).unwrap();
        hub.enqueue(spec(Some(&small)), c.player.clone()).unwrap();
        assert_eq!(hub.status().queued, 2);
        hub.enqueue(spec(Some(&json!({}))), b.player.clone())
            .unwrap();

        assert!(matches!(a.recv().await, ServerMessage::Queued { .. }));
        let (ga, sa, _) = a.game_start().await;
        let (gb, sb, _) = b.game_start().await;
        assert_eq!(ga, gb);
        assert_ne!(sa, sb);
        // The bot with a different config is still waiting; leaving clears it.
        assert_eq!(hub.status().queued, 1);
        assert!(matches!(c.recv().await, ServerMessage::Queued { .. }));
        hub.dequeue(c.player.outbox.conn);
        assert_eq!(hub.status().queued, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn spec_validation() {
        let (hub, _) = hub();
        fn req<'a>(
            game: &'a str,
            config: Option<&'a serde_json::Value>,
            think: Option<u64>,
        ) -> GameRequest<'a> {
            GameRequest {
                game,
                config,
                think_time_ms: think,
                ..GameRequest::default()
            }
        }
        assert!(matches!(
            hub.spec(req("chess", None, None)),
            Err(HubError::UnknownGame(_))
        ));
        assert!(matches!(
            hub.spec(req("connect4", Some(&json!({"rows": 99})), None)),
            Err(HubError::InvalidConfig(_))
        ));
        assert!(matches!(
            hub.spec(req("connect4", None, Some(1))),
            Err(HubError::InvalidConfig(_))
        ));
        assert!(matches!(
            hub.spec(GameRequest {
                game: "tictactoe",
                seats: Some(SeatCount::new(3).unwrap()),
                ..GameRequest::default()
            }),
            Err(HubError::InvalidConfig(_))
        ));
        let spec = hub.spec(req("connect4", None, None)).unwrap();
        assert_eq!(spec.config["columns"], json!(7));
        assert_eq!(spec.think_time, ThinkTime::DEFAULT);

        let (busy, _) = hub_with(ServerConfig {
            max_games: 0,
            ..ServerConfig::default()
        });
        let spec = busy.spec(req("tictactoe", None, None)).unwrap();
        assert_eq!(busy.create_game(spec).unwrap_err(), HubError::ServerBusy);
    }
}
