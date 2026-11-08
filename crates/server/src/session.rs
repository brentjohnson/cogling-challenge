//! The game session actor: one tokio task per game, owning its state.
//!
//! Connections talk to a session only through [`Command`]s; the session talks
//! back only through each player's [`Outbox`]. Because the task is the sole
//! owner of the authoritative state, no locks guard game state, and every
//! decision about timing (deadlines, grace periods) is made in one place.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use cogling_eventlog::{
    EventBody, Header, HeaderKind, LOG_FORMAT, LogSink, LogStore, PlayerRecord, SeatStats, Source,
    unix_ms,
};
use cogling_game_core::{
    DynError, DynGame, DynState, ForfeitPolicy, ForfeitReason, GameResult, IllegalAction, Seat,
    SeatCount, Seed, Status,
};
use cogling_protocol::{
    CoglingInfo, DecisionId, ErrorCode, GameId, ObservationMode, PlayerSlot, RejectCode,
    ResumeToken, ServerMessage,
};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until};
use tracing::{debug, info, warn};

use crate::config::ThinkTime;

/// Identifies one WebSocket connection (or test harness participant).
pub(crate) type ConnId = u64;

/// A session's handle on a connection's outbound queue.
///
/// Sends never block the session: if a cogling stops reading and its queue
/// fills, messages are dropped (it will then miss its deadline).
#[derive(Debug, Clone)]
pub(crate) struct Outbox {
    pub(crate) conn: ConnId,
    tx: mpsc::Sender<ServerMessage>,
}

impl Outbox {
    pub(crate) const fn new(conn: ConnId, tx: mpsc::Sender<ServerMessage>) -> Self {
        Self { conn, tx }
    }

    pub(crate) fn send(&self, msg: ServerMessage) {
        if let Err(e) = self.tx.try_send(msg) {
            debug!(conn = self.conn, error = %e, "dropping outbound message");
        }
    }
}

/// A cogling taking part in a session.
#[derive(Debug, Clone)]
pub(crate) struct Participant {
    pub(crate) cogling: CoglingInfo,
    pub(crate) outbox: Outbox,
}

/// Everything fixed about a game before it starts.
#[derive(Clone)]
pub(crate) struct GameSpec {
    pub(crate) game: Arc<dyn DynGame>,
    pub(crate) seats: SeatCount,
    /// Canonical configuration JSON.
    pub(crate) config: Value,
    pub(crate) think_time: ThinkTime,
    pub(crate) observation_mode: ObservationMode,
}

/// Live sessions by id. Sessions remove themselves when they end.
pub(crate) type Directory = Arc<Mutex<HashMap<GameId, GameHandle>>>;

/// Server-wide context shared by sessions.
#[derive(Clone)]
pub(crate) struct SessionEnv {
    pub(crate) directory: Directory,
    pub(crate) logs: Arc<dyn LogStore>,
    pub(crate) reconnect_grace: Duration,
    pub(crate) lobby_timeout: Duration,
    pub(crate) forfeit_policy: ForfeitPolicy,
}

/// Sends commands to a session.
#[derive(Debug, Clone)]
pub(crate) struct GameHandle {
    tx: mpsc::Sender<Command>,
}

impl GameHandle {
    /// Delivers a command; `false` if the session has ended.
    pub(crate) async fn send(&self, cmd: Command) -> bool {
        self.tx.send(cmd).await.is_ok()
    }
}

/// Why a join failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JoinError {
    /// All seats are taken or the game already started.
    Full,
}

/// Why a reconnect failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReconnectError {
    /// No seat has that token, or the game is not running.
    InvalidToken,
}

/// Messages to a session.
#[derive(Debug)]
pub(crate) enum Command {
    Join {
        player: Participant,
        reply: oneshot::Sender<Result<(), JoinError>>,
    },
    Action {
        from: Outbox,
        seat: Seat,
        decision_id: DecisionId,
        action: Value,
        /// When the frame arrived at the server; used for timing so that
        /// session scheduling delays never count against the cogling.
        received: Instant,
    },
    Disconnected {
        conn: ConnId,
    },
    Reconnect {
        token: ResumeToken,
        player: Participant,
        reply: oneshot::Sender<Result<Seat, ReconnectError>>,
    },
}

/// Starts a session task and registers it in the directory.
///
/// If `players` already fills every seat (quick-match), the game starts
/// immediately; otherwise it waits in the lobby for `join`s.
pub(crate) fn spawn(
    id: GameId,
    spec: GameSpec,
    env: SessionEnv,
    players: Vec<Participant>,
) -> GameHandle {
    let (tx, rx) = mpsc::channel(256);
    let handle = GameHandle { tx };
    env.directory
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(id, handle.clone());
    let expires = Instant::now() + env.lobby_timeout;
    let session = Session {
        ctx: Ctx { id, spec, env },
        lobby: players,
        phase: Phase::Forming { expires },
    };
    tokio::spawn(session.run(rx));
    handle
}

/// `Err(result)` means the game is over with that result.
type Step = Result<(), GameResult>;

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn aborted(reason: impl Into<String>) -> GameResult {
    GameResult::Aborted {
        reason: reason.into(),
    }
}

fn internal(e: &DynError) -> GameResult {
    warn!(error = %e, "engine error; aborting game");
    aborted(format!("internal engine error: {e}"))
}

fn new_token() -> ResumeToken {
    ResumeToken::new(format!(
        "{:032x}{:032x}",
        rand::random::<u128>(),
        rand::random::<u128>()
    ))
}

const fn reject_code(e: &DynError) -> RejectCode {
    match e {
        DynError::MalformedAction(_) => RejectCode::MalformedAction,
        DynError::Illegal(IllegalAction::NotYourTurn(_)) => RejectCode::NotYourTurn,
        DynError::Illegal(IllegalAction::GameFinished) => RejectCode::GameFinished,
        _ => RejectCode::IllegalAction,
    }
}

struct Ctx {
    id: GameId,
    spec: GameSpec,
    env: SessionEnv,
}

struct Pending {
    id: DecisionId,
    sent_at: Instant,
    deadline: Instant,
}

struct SeatSlot {
    cogling: CoglingInfo,
    conn: Option<Outbox>,
    token: ResumeToken,
    disconnected_at: Option<Instant>,
    pending: Option<Pending>,
    stats: SeatStats,
}

struct Running {
    state: Box<dyn DynState>,
    started: Instant,
    log: Box<dyn LogSink>,
    seats: Vec<SeatSlot>,
    next_decision: DecisionId,
}

enum Phase {
    Forming { expires: Instant },
    Running(Box<Running>),
    Over,
}

struct Session {
    ctx: Ctx,
    lobby: Vec<Participant>,
    phase: Phase,
}

impl Session {
    async fn run(mut self, mut rx: mpsc::Receiver<Command>) {
        if self.lobby.len() >= self.ctx.spec.seats.len()
            && let Err(result) = self.start()
        {
            self.finish(result);
            return;
        }
        loop {
            let wake = self.next_wakeup();
            let step = tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(cmd) => self.handle(cmd),
                    // Unreachable while registered (the directory holds a sender).
                    None => Err(aborted("session channel closed")),
                },
                () = sleep_until(wake.unwrap_or_else(far_future)), if wake.is_some() => {
                    self.on_timer(Instant::now())
                }
            };
            if let Err(result) = step {
                self.finish(result);
                return;
            }
        }
    }

    fn next_wakeup(&self) -> Option<Instant> {
        match &self.phase {
            Phase::Forming { expires } => Some(*expires),
            Phase::Running(r) => r.next_wakeup(self.ctx.env.reconnect_grace),
            Phase::Over => None,
        }
    }

    fn handle(&mut self, cmd: Command) -> Step {
        match cmd {
            Command::Join { player, reply } => self.on_join(player, reply),
            Command::Action {
                from,
                seat,
                decision_id,
                action,
                received,
            } => match &mut self.phase {
                Phase::Running(r) => {
                    r.on_action(&self.ctx, &from, seat, decision_id, &action, received)
                }
                Phase::Forming { .. } | Phase::Over => {
                    from.send(ServerMessage::error(
                        ErrorCode::UnexpectedMessage,
                        "the game has not started",
                        false,
                    ));
                    Ok(())
                }
            },
            Command::Disconnected { conn } => match &mut self.phase {
                Phase::Forming { .. } => {
                    self.lobby.retain(|p| p.outbox.conn != conn);
                    Ok(())
                }
                Phase::Running(r) => r.on_disconnected(conn),
                Phase::Over => Ok(()),
            },
            Command::Reconnect {
                token,
                player,
                reply,
            } => match &mut self.phase {
                Phase::Running(r) => r.on_reconnect(&self.ctx, &token, player, reply),
                Phase::Forming { .. } | Phase::Over => {
                    let _ = reply.send(Err(ReconnectError::InvalidToken));
                    Ok(())
                }
            },
        }
    }

    fn on_join(
        &mut self,
        player: Participant,
        reply: oneshot::Sender<Result<(), JoinError>>,
    ) -> Step {
        let seats = self.ctx.spec.seats.len();
        let full = !matches!(self.phase, Phase::Forming { .. }) || self.lobby.len() >= seats;
        if full {
            let _ = reply.send(Err(JoinError::Full));
            return Ok(());
        }
        self.lobby.push(player);
        let _ = reply.send(Ok(()));
        let filled = u8::try_from(self.lobby.len()).unwrap_or(u8::MAX);
        for p in &self.lobby {
            p.outbox.send(ServerMessage::Joined {
                game_id: self.ctx.id,
                seats_filled: filled,
                seats_total: self.ctx.spec.seats.get(),
            });
        }
        if self.lobby.len() >= seats {
            self.start()?;
        }
        Ok(())
    }

    fn start(&mut self) -> Step {
        let spec = &self.ctx.spec;
        let seed = Seed::new(rand::random());
        let state = spec
            .game
            .create(Some(&spec.config), spec.seats, seed)
            .map_err(|e| internal(&e))?;
        let info = spec.game.info();
        let players: Vec<PlayerRecord> = spec
            .seats
            .seats()
            .zip(&self.lobby)
            .map(|(seat, p)| PlayerRecord {
                seat,
                cogling: p.cogling.clone(),
            })
            .collect();
        let header = Header {
            kind: HeaderKind::Header,
            log_format: LOG_FORMAT,
            game_id: self.ctx.id,
            game: info.id.to_owned(),
            engine_version: info.engine_version.to_owned(),
            ruleset: info.ruleset.to_owned(),
            config: spec.config.clone(),
            seats: spec.seats,
            seed,
            players,
            source: Source::Server,
            think_time_ms: Some(spec.think_time.as_millis()),
            observation_mode: Some(spec.observation_mode),
            started_at_ms: unix_ms(),
        };
        let log = self.ctx.env.logs.create(&header).map_err(|e| {
            warn!(error = %e, "cannot create event log");
            aborted("event log unavailable")
        })?;

        let seats: Vec<SeatSlot> = spec
            .seats
            .seats()
            .zip(self.lobby.drain(..))
            .map(|(seat, p)| SeatSlot {
                cogling: p.cogling,
                conn: Some(p.outbox),
                token: new_token(),
                disconnected_at: None,
                pending: None,
                stats: SeatStats::new(seat),
            })
            .collect();
        let mut running = Box::new(Running {
            state,
            started: Instant::now(),
            log,
            seats,
            next_decision: DecisionId::FIRST,
        });
        info!(game_id = %self.ctx.id, game = info.id, "game started");
        for seat in spec.seats.seats() {
            running.send_game_start(&self.ctx, seat);
        }
        let step = running.advance(&self.ctx);
        self.phase = Phase::Running(running);
        step
    }

    fn on_timer(&mut self, now: Instant) -> Step {
        match &mut self.phase {
            Phase::Forming { expires } if now >= *expires => Err(aborted("lobby expired")),
            Phase::Running(r) => r.on_timer(&self.ctx, now),
            Phase::Forming { .. } | Phase::Over => Ok(()),
        }
    }

    fn finish(&mut self, result: GameResult) {
        match std::mem::replace(&mut self.phase, Phase::Over) {
            Phase::Forming { .. } => {
                for p in self.lobby.drain(..) {
                    p.outbox.send(ServerMessage::GameOver {
                        game_id: self.ctx.id,
                        result: result.clone(),
                        observation: Value::Null,
                    });
                }
            }
            Phase::Running(mut r) => r.finish(&self.ctx, result),
            Phase::Over => {}
        }
        self.ctx
            .env
            .directory
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.ctx.id);
    }
}

fn far_future() -> Instant {
    Instant::now() + Duration::from_secs(86_400 * 365)
}

impl Running {
    fn next_wakeup(&self, grace: Duration) -> Option<Instant> {
        self.seats
            .iter()
            .flat_map(|s| {
                [
                    s.pending.as_ref().map(|p| p.deadline),
                    s.disconnected_at.map(|t| t + grace),
                ]
            })
            .flatten()
            .min()
    }

    fn log(&mut self, body: EventBody) -> Step {
        let t_ms = ms(self.started.elapsed());
        self.log.append(t_ms, body).map(drop).map_err(|e| {
            warn!(error = %e, "event log write failed");
            aborted("event log write failed")
        })
    }

    fn forfeit(&self, ctx: &Ctx, seat: Seat, reason: ForfeitReason) -> GameResult {
        GameResult::forfeit(
            seat,
            reason,
            ctx.spec.seats,
            ctx.env.forfeit_policy,
            self.state.provisional_ranks().as_deref(),
        )
    }

    fn send_game_start(&self, ctx: &Ctx, seat: Seat) {
        let Some(slot) = self.seats.get(seat.index()) else {
            return;
        };
        let Some(outbox) = &slot.conn else {
            return;
        };
        let info = ctx.spec.game.info();
        outbox.send(ServerMessage::GameStart {
            game_id: ctx.id,
            game: info.id.to_owned(),
            engine_version: info.engine_version.to_owned(),
            ruleset: info.ruleset.to_owned(),
            config: ctx.spec.config.clone(),
            seat,
            players: self
                .seats
                .iter()
                .zip(ctx.spec.seats.seats())
                .map(|(s, seat)| PlayerSlot {
                    seat,
                    cogling: s.cogling.clone(),
                })
                .collect(),
            resume_token: slot.token.clone(),
            observation_mode: ctx.spec.observation_mode,
            think_time_ms: ctx.spec.think_time.as_millis(),
        });
    }

    fn send_decide(&self, ctx: &Ctx, seat: Seat, now: Instant) -> Step {
        let Some(slot) = self.seats.get(seat.index()) else {
            return Ok(());
        };
        let (Some(outbox), Some(pending)) = (&slot.conn, &slot.pending) else {
            return Ok(());
        };
        let observation = self.state.player_view(seat).map_err(|e| internal(&e))?;
        let legal_actions = match ctx.spec.observation_mode {
            ObservationMode::State => None,
            ObservationMode::StateAndLegal => {
                Some(self.state.legal_actions(seat).map_err(|e| internal(&e))?)
            }
        };
        outbox.send(ServerMessage::Decide {
            game_id: ctx.id,
            decision_id: pending.id,
            remaining_ms: ms(pending.deadline.saturating_duration_since(now)),
            observation,
            legal_actions,
        });
        Ok(())
    }

    /// Issues decisions to every seat that must act and has none pending,
    /// or ends the game.
    fn advance(&mut self, ctx: &Ctx) -> Step {
        let active = match self.state.status().map_err(|e| internal(&e))? {
            Status::Finished(outcome) => return Err(GameResult::completed(outcome)),
            Status::ToAct(active) => active,
        };
        let now = Instant::now();
        for seat in active.iter() {
            let Some(slot) = self.seats.get_mut(seat.index()) else {
                continue;
            };
            if slot.pending.is_some() {
                continue;
            }
            slot.pending = Some(Pending {
                id: self.next_decision,
                sent_at: now,
                deadline: now + ctx.spec.think_time.duration(),
            });
            self.next_decision = self.next_decision.next();
            self.send_decide(ctx, seat, now)?;
        }
        Ok(())
    }

    fn on_action(
        &mut self,
        ctx: &Ctx,
        from: &Outbox,
        seat: Seat,
        decision_id: DecisionId,
        action: &Value,
        received: Instant,
    ) -> Step {
        let Some(slot) = self.seats.get_mut(seat.index()) else {
            return Ok(());
        };
        if slot.conn.as_ref().map(|o| o.conn) != Some(from.conn) {
            from.send(ServerMessage::error(
                ErrorCode::Superseded,
                "this seat is controlled by another connection",
                true,
            ));
            return Ok(());
        }
        let reject = |code, message: String, remaining_ms| ServerMessage::ActionRejected {
            game_id: ctx.id,
            decision_id,
            code,
            message,
            remaining_ms,
        };
        let now = Instant::now();
        let Some(pending) = slot.pending.as_ref().filter(|p| p.id == decision_id) else {
            let remaining = slot
                .pending
                .as_ref()
                .map_or(0, |p| ms(p.deadline.saturating_duration_since(now)));
            from.send(reject(
                RejectCode::StaleDecision,
                format!("decision {decision_id} is not pending"),
                remaining,
            ));
            return Ok(());
        };
        if received > pending.deadline {
            return self.timeout(ctx, seat);
        }
        let elapsed_ms = ms(received.saturating_duration_since(pending.sent_at));
        let remaining_ms = ms(pending.deadline.saturating_duration_since(now));

        match self.state.apply(seat, action) {
            Err(e) => {
                slot.stats.rejected += 1;
                let code = reject_code(&e);
                let message = e.to_string();
                from.send(reject(code, message.clone(), remaining_ms));
                self.log(EventBody::Rejected {
                    seat,
                    decision_id,
                    action: action.clone(),
                    code,
                    message,
                    elapsed_ms,
                })
            }
            Ok(()) => {
                slot.pending = None;
                slot.stats.record_action(elapsed_ms);
                from.send(ServerMessage::ActionAccepted {
                    game_id: ctx.id,
                    decision_id,
                });
                let state_hash = self.state.state_hash().map_err(|e| internal(&e))?;
                self.log(EventBody::Action {
                    seat,
                    decision_id: Some(decision_id),
                    action: action.clone(),
                    response_ms: Some(elapsed_ms),
                    state_hash,
                })?;
                self.advance(ctx)
            }
        }
    }

    fn timeout(&mut self, ctx: &Ctx, seat: Seat) -> Step {
        let Some(slot) = self.seats.get_mut(seat.index()) else {
            return Ok(());
        };
        let Some(pending) = slot.pending.take() else {
            return Ok(());
        };
        slot.stats.timeouts += 1;
        info!(game_id = %ctx.id, %seat, "decision deadline expired");
        self.log(EventBody::Timeout {
            seat,
            decision_id: pending.id,
        })?;
        Err(self.forfeit(ctx, seat, ForfeitReason::Timeout))
    }

    fn on_timer(&mut self, ctx: &Ctx, now: Instant) -> Step {
        let grace = ctx.env.reconnect_grace;
        let expired = self
            .seats
            .iter()
            .zip(ctx.spec.seats.seats())
            .find_map(|(s, seat)| {
                if s.pending.as_ref().is_some_and(|p| now >= p.deadline) {
                    Some((seat, ForfeitReason::Timeout))
                } else if s.disconnected_at.is_some_and(|t| now >= t + grace) {
                    Some((seat, ForfeitReason::Disconnected))
                } else {
                    None
                }
            });
        match expired {
            Some((seat, ForfeitReason::Timeout)) => self.timeout(ctx, seat),
            Some((seat, reason)) => {
                info!(game_id = %ctx.id, %seat, "reconnect grace period expired");
                Err(self.forfeit(ctx, seat, reason))
            }
            None => Ok(()),
        }
    }

    fn on_disconnected(&mut self, conn: ConnId) -> Step {
        let found = self
            .seats
            .iter_mut()
            .zip(0_u8..)
            .find(|(s, _)| s.conn.as_ref().is_some_and(|o| o.conn == conn));
        let Some((slot, index)) = found else {
            return Ok(());
        };
        slot.conn = None;
        slot.disconnected_at = Some(Instant::now());
        slot.stats.disconnects += 1;
        let Ok(seat) = Seat::new(index) else {
            return Ok(());
        };
        self.log(EventBody::Disconnect { seat })
    }

    fn on_reconnect(
        &mut self,
        ctx: &Ctx,
        token: &ResumeToken,
        player: Participant,
        reply: oneshot::Sender<Result<Seat, ReconnectError>>,
    ) -> Step {
        let found = self
            .seats
            .iter_mut()
            .zip(ctx.spec.seats.seats())
            .find(|(s, _)| s.token == *token);
        let Some((slot, seat)) = found else {
            let _ = reply.send(Err(ReconnectError::InvalidToken));
            return Ok(());
        };
        if let Some(old) = slot.conn.replace(player.outbox) {
            old.send(ServerMessage::error(
                ErrorCode::Superseded,
                "this seat was reclaimed by a new connection",
                true,
            ));
        }
        slot.disconnected_at = None;
        let _ = reply.send(Ok(seat));
        self.log(EventBody::Reconnect { seat })?;
        self.send_game_start(ctx, seat);
        self.send_decide(ctx, seat, Instant::now())
    }

    fn finish(&mut self, ctx: &Ctx, result: GameResult) {
        let stats: Vec<SeatStats> = self.seats.iter().map(|s| s.stats).collect();
        let t_ms = ms(self.started.elapsed());
        if let Err(e) = self.log.append(
            t_ms,
            EventBody::End {
                result: result.clone(),
                stats,
            },
        ) {
            warn!(error = %e, "failed to write end event");
        }
        // Replace the sink so it can be closed by value.
        let log = std::mem::replace(&mut self.log, Box::new(ClosedSink));
        if let Err(e) = log.close() {
            warn!(error = %e, "failed to close event log");
        }
        for (slot, seat) in self.seats.iter().zip(ctx.spec.seats.seats()) {
            if let Some(outbox) = &slot.conn {
                outbox.send(ServerMessage::GameOver {
                    game_id: ctx.id,
                    result: result.clone(),
                    observation: self.state.player_view(seat).unwrap_or(Value::Null),
                });
            }
        }
        info!(game_id = %ctx.id, ?result, "game over");
    }
}

/// Placeholder left behind after the real sink is closed.
struct ClosedSink;

impl LogSink for ClosedSink {
    fn append(&mut self, _t_ms: u64, _body: EventBody) -> Result<u64, cogling_eventlog::LogError> {
        Ok(0)
    }

    fn close(self: Box<Self>) -> Result<(), cogling_eventlog::LogError> {
        Ok(())
    }
}
