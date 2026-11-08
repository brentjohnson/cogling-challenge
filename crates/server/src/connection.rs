//! One WebSocket connection: framing, limits and the per-connection state
//! machine (`hello` → idle → queued/lobby → in game → idle ...).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use cogling_game_core::Seat;
use cogling_protocol::{
    ClientMessage, CoglingInfo, ErrorCode, GameId, PROTOCOL_VERSION, ServerMessage,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};
use tracing::{debug, info};

use crate::hub::{ConnectionGuard, GameRequest, Hub};
use crate::session::{Command, ConnId, GameHandle, Outbox, Participant};

/// Outbound queue depth per connection.
const OUTBOX_CAPACITY: usize = 256;

/// Token bucket allowing `rate` messages per second with bursts of `2 * rate`.
#[derive(Debug)]
struct TokenBucket {
    tokens: f64,
    capacity: f64,
    rate: f64,
    last: Instant,
}

impl TokenBucket {
    fn new(rate: u32, now: Instant) -> Self {
        let rate = f64::from(rate.max(1));
        Self {
            tokens: rate * 2.0,
            capacity: rate * 2.0,
            rate,
            last: now,
        }
    }

    fn allow(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[derive(Debug)]
enum Phase {
    AwaitHello,
    Idle,
    Queued,
    Lobby {
        game_id: GameId,
        handle: GameHandle,
    },
    InGame {
        game_id: GameId,
        seat: Seat,
        handle: GameHandle,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Close,
}

struct Conn {
    hub: Arc<Hub>,
    conn: ConnId,
    outbox: Outbox,
    phase: Phase,
    cogling: Option<CoglingInfo>,
    violations: u32,
    bucket: TokenBucket,
}

impl Conn {
    fn send(&self, msg: ServerMessage) {
        self.outbox.send(msg);
    }

    fn participant(&self) -> Option<Participant> {
        self.cogling.clone().map(|cogling| Participant {
            cogling,
            outbox: self.outbox.clone(),
        })
    }

    fn violation(&mut self, code: ErrorCode, message: String) -> Flow {
        self.violations += 1;
        let fatal = self.violations > self.hub.config.max_violations;
        debug!(conn = self.conn, ?code, %message, fatal, "protocol violation");
        self.send(ServerMessage::error(code, message, fatal));
        if fatal { Flow::Close } else { Flow::Continue }
    }

    fn unexpected(&self, message: &str) -> Flow {
        self.send(ServerMessage::error(
            ErrorCode::UnexpectedMessage,
            message,
            false,
        ));
        Flow::Continue
    }

    async fn on_text(&mut self, text: &str, received: Instant) -> Flow {
        if !self.bucket.allow(received) {
            return self.violation(ErrorCode::RateLimited, "rate limit exceeded".into());
        }
        match ClientMessage::from_json(text) {
            Ok(msg) => self.on_message(msg, received).await,
            Err(e) => self.violation(ErrorCode::Malformed, e.to_string()),
        }
    }

    async fn on_message(&mut self, msg: ClientMessage, received: Instant) -> Flow {
        if matches!(self.phase, Phase::AwaitHello) {
            return self.on_hello(msg);
        }
        match msg {
            ClientMessage::Hello { .. } => self.unexpected("already said hello"),
            ClientMessage::Ping { nonce } => {
                self.send(ServerMessage::Pong { nonce });
                Flow::Continue
            }
            ClientMessage::CreateGame {
                game,
                seats,
                config,
                think_time_ms,
                observation_mode,
            } => {
                let req = GameRequest {
                    game: &game,
                    seats,
                    config: config.as_ref(),
                    think_time_ms,
                    observation_mode,
                };
                match self
                    .hub
                    .spec(req)
                    .and_then(|spec| self.hub.create_game(spec))
                {
                    Ok(game_id) => self.send(ServerMessage::GameCreated { game_id }),
                    Err(e) => self.send(e.to_message()),
                }
                Flow::Continue
            }
            ClientMessage::Join { game_id } => self.on_join(game_id).await,
            ClientMessage::Queue {
                game,
                seats,
                config,
            } => {
                if !matches!(self.phase, Phase::Idle) {
                    return self.unexpected("already queued or in a game");
                }
                let Some(player) = self.participant() else {
                    return Flow::Close;
                };
                let req = GameRequest {
                    game: &game,
                    seats,
                    config: config.as_ref(),
                    ..GameRequest::default()
                };
                match self
                    .hub
                    .spec(req)
                    .and_then(|spec| self.hub.enqueue(spec, player))
                {
                    Ok(()) => self.phase = Phase::Queued,
                    Err(e) => self.send(e.to_message()),
                }
                Flow::Continue
            }
            ClientMessage::LeaveQueue => {
                if !matches!(self.phase, Phase::Queued) {
                    return self.unexpected("not queued");
                }
                self.hub.dequeue(self.conn);
                self.phase = Phase::Idle;
                Flow::Continue
            }
            ClientMessage::Action {
                game_id,
                decision_id,
                action,
            } => {
                let Phase::InGame {
                    game_id: current,
                    seat,
                    handle,
                } = &self.phase
                else {
                    return self.unexpected("not in a game");
                };
                if *current != game_id {
                    return self.unexpected("action for a different game");
                }
                let cmd = Command::Action {
                    from: self.outbox.clone(),
                    seat: *seat,
                    decision_id,
                    action,
                    received,
                };
                if !handle.send(cmd).await {
                    self.phase = Phase::Idle;
                    self.send(ServerMessage::error(
                        ErrorCode::NoSuchGame,
                        "the game has ended",
                        false,
                    ));
                }
                Flow::Continue
            }
            ClientMessage::Reconnect {
                game_id,
                resume_token,
            } => {
                if !matches!(self.phase, Phase::Idle) {
                    return self.unexpected("already queued or in a game");
                }
                let Some(player) = self.participant() else {
                    return Flow::Close;
                };
                match self.hub.reconnect(game_id, resume_token, player).await {
                    Ok((handle, seat)) => {
                        info!(conn = self.conn, %game_id, %seat, "cogling reconnected");
                        self.phase = Phase::InGame {
                            game_id,
                            seat,
                            handle,
                        };
                    }
                    Err(e) => self.send(e.to_message()),
                }
                Flow::Continue
            }
            // `ClientMessage` is non-exhaustive: tolerate future additions.
            _ => self.unexpected("unsupported message"),
        }
    }

    fn on_hello(&mut self, msg: ClientMessage) -> Flow {
        let ClientMessage::Hello {
            protocol, cogling, ..
        } = msg
        else {
            self.send(ServerMessage::error(
                ErrorCode::HelloRequired,
                "the first message must be hello",
                true,
            ));
            return Flow::Close;
        };
        if protocol != PROTOCOL_VERSION {
            self.send(ServerMessage::error(
                ErrorCode::ProtocolVersion,
                format!("server speaks protocol {PROTOCOL_VERSION}, cogling sent {protocol}"),
                true,
            ));
            return Flow::Close;
        }
        info!(conn = self.conn, %cogling, "cogling connected");
        self.cogling = Some(cogling);
        self.phase = Phase::Idle;
        self.send(ServerMessage::Welcome {
            protocol: PROTOCOL_VERSION,
            server_version: env!("CARGO_PKG_VERSION").to_owned(),
            limits: self.hub.config.limits(),
        });
        Flow::Continue
    }

    async fn on_join(&mut self, game_id: GameId) -> Flow {
        if !matches!(self.phase, Phase::Idle) {
            return self.unexpected("already queued or in a game");
        }
        let Some(player) = self.participant() else {
            return Flow::Close;
        };
        match self.hub.join(game_id, player).await {
            Ok(handle) => self.phase = Phase::Lobby { game_id, handle },
            Err(e) => self.send(e.to_message()),
        }
        Flow::Continue
    }

    /// Tracks phase changes driven by the session.
    fn observe_outgoing(&mut self, msg: &ServerMessage) {
        match msg {
            ServerMessage::GameStart { game_id, seat, .. } => {
                if let Some(handle) = self.hub.handle(*game_id) {
                    self.phase = Phase::InGame {
                        game_id: *game_id,
                        seat: *seat,
                        handle,
                    };
                }
            }
            ServerMessage::GameOver { game_id, .. } => {
                let ours = match &self.phase {
                    Phase::Lobby { game_id: g, .. } | Phase::InGame { game_id: g, .. } => {
                        g == game_id
                    }
                    _ => false,
                };
                if ours {
                    self.phase = Phase::Idle;
                }
            }
            _ => {}
        }
    }

    async fn on_close(&mut self) {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Queued => self.hub.dequeue(self.conn),
            Phase::Lobby { handle, .. } | Phase::InGame { handle, .. } => {
                handle.send(Command::Disconnected { conn: self.conn }).await;
            }
            Phase::AwaitHello | Phase::Idle => {}
        }
    }
}

async fn send_frame(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    msg: &ServerMessage,
) -> bool {
    match msg.to_json() {
        Ok(text) => sink.send(Message::Text(text.into())).await.is_ok(),
        Err(e) => {
            debug!(error = %e, "cannot serialize outbound message");
            true
        }
    }
}

/// Drives one WebSocket until it closes.
pub(crate) async fn handle_socket(hub: Arc<Hub>, socket: WebSocket, _guard: ConnectionGuard) {
    let conn_id = hub.next_conn_id();
    let (tx, mut out_rx) = mpsc::channel(OUTBOX_CAPACITY);
    let now = Instant::now();
    let hello_deadline = now + hub.config.hello_timeout;
    let mut conn = Conn {
        conn: conn_id,
        outbox: Outbox::new(conn_id, tx),
        phase: Phase::AwaitHello,
        cogling: None,
        violations: 0,
        bucket: TokenBucket::new(hub.config.max_messages_per_second, now),
        hub,
    };
    let (mut sink, mut stream) = socket.split();

    loop {
        let awaiting_hello = matches!(conn.phase, Phase::AwaitHello);
        let flow = tokio::select! {
            frame = stream.next() => {
                let received = Instant::now();
                match frame {
                    Some(Ok(Message::Text(text))) => conn.on_text(text.as_str(), received).await,
                    Some(Ok(Message::Binary(_))) => {
                        conn.violation(ErrorCode::Malformed, "binary frames are not supported".into())
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => Flow::Continue,
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                }
            }
            Some(msg) = out_rx.recv() => {
                conn.observe_outgoing(&msg);
                let fatal = matches!(msg, ServerMessage::Error { fatal: true, .. });
                if !send_frame(&mut sink, &msg).await || fatal {
                    break;
                }
                Flow::Continue
            }
            () = sleep_until(hello_deadline), if awaiting_hello => {
                conn.send(ServerMessage::error(ErrorCode::HelloRequired, "hello timeout", true));
                Flow::Close
            }
        };
        if flow == Flow::Close {
            break;
        }
    }

    // Deliver anything already queued (e.g. a fatal error explaining the close).
    while let Ok(msg) = out_rx.try_recv() {
        if !send_frame(&mut sink, &msg).await {
            break;
        }
    }
    conn.on_close().await;
    let _ = tokio::time::timeout(Duration::from_secs(1), sink.close()).await;
    debug!(conn = conn_id, "connection closed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_bucket_limits_bursts_and_refills() {
        let start = Instant::now();
        let mut b = TokenBucket::new(5, start);
        let allowed = (0..20).filter(|_| b.allow(start)).count();
        assert_eq!(allowed, 10);
        assert!(!b.allow(start));
        assert!(b.allow(start + Duration::from_millis(250)));
    }
}
