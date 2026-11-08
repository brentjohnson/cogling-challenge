//! Rust client SDK for writing coglings.
//!
//! Implement [`Cogling`] (usually just [`Cogling::decide`]) and call [`run`]. The SDK
//! handles the protocol: `hello`, matchmaking, decisions, retries after
//! rejected actions, reconnecting after a dropped connection, and playing
//! several games in a row.
//!
//! # Examples
//!
//! ```no_run
//! use cogling_sdk::{Cogling, CoglingError, ClientConfig, Decision, Matchmaking, run};
//! use cogling_protocol::CoglingInfo;
//! use serde_json::Value;
//!
//! struct FirstLegal;
//!
//! impl Cogling for FirstLegal {
//!     fn decide(&mut self, d: &Decision) -> Result<Value, CoglingError> {
//!         d.legal_actions()?.first().cloned().ok_or(CoglingError::NoLegalActions)
//!     }
//! }
//!
//! # async fn go() -> Result<(), Box<dyn std::error::Error>> {
//! let config = ClientConfig::new(
//!     "ws://127.0.0.1:7878/ws",
//!     CoglingInfo::new("first-legal", Some("1.0"))?,
//!     Matchmaking::queue("tictactoe"),
//! );
//! let games = run(&config, &mut FirstLegal).await?;
//! # Ok(()) }
//! ```

pub mod strategies;

use std::time::Duration;

use cogling_game_core::{GameResult, Seat, SeatCount};
use cogling_protocol::{
    ApiKey, ClientMessage, CoglingInfo, DecisionId, ErrorCode, GameId, ObservationMode,
    PROTOCOL_VERSION, RejectCode, ResumeToken, ServerMessage,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{debug, info, warn};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Reconnect attempts after a dropped connection mid-game.
const RECONNECT_ATTEMPTS: u32 = 5;

/// A cogling's decision failed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CoglingError {
    /// The strategy needs legal actions but the game uses `state` mode.
    #[error("this cogling needs legal actions; use observation mode state_and_legal")]
    LegalActionsRequired,
    /// There was nothing to choose from.
    #[error("no legal actions available")]
    NoLegalActions,
    /// The observation did not have the expected shape.
    #[error("unexpected observation: {0}")]
    Observation(String),
}

/// The SDK failed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ClientError {
    /// Connecting or talking to the server failed.
    #[error("websocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    /// The server reported a fatal or blocking error.
    #[error("server error {code:?}: {message}")]
    Server {
        /// Error code.
        code: ErrorCode,
        /// Explanation.
        message: String,
    },
    /// The server sent something unexpected.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The connection closed and could not be resumed.
    #[error("connection closed")]
    Closed,
}

/// Static information about the current game.
#[derive(Debug, Clone, PartialEq)]
pub struct GameInfo {
    /// Session id.
    pub game_id: GameId,
    /// Game type id, e.g. `"connect4"`.
    pub game: String,
    /// Canonical configuration.
    pub config: Value,
    /// Our seat.
    pub seat: Seat,
    /// Seat count.
    pub seats: usize,
    /// Think time per decision.
    pub think_time_ms: u64,
    /// Observation mode.
    pub observation_mode: ObservationMode,
}

/// A request to act.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// Our seat.
    pub seat: Seat,
    /// Decision id to echo.
    pub decision_id: DecisionId,
    /// Time left, as measured by the server when it sent the message.
    pub remaining_ms: u64,
    /// The full player observation.
    pub observation: Value,
    /// Legal actions, in `state_and_legal` mode.
    pub legal_actions: Option<Vec<Value>>,
}

impl Decision {
    /// The legal actions.
    ///
    /// # Errors
    ///
    /// [`CoglingError::LegalActionsRequired`] in `state` mode.
    pub fn legal_actions(&self) -> Result<&[Value], CoglingError> {
        self.legal_actions
            .as_deref()
            .ok_or(CoglingError::LegalActionsRequired)
    }
}

/// A game-playing strategy.
pub trait Cogling: Send {
    /// Called when a game starts (or resumes after a reconnect).
    fn on_game_start(&mut self, _info: &GameInfo) {}

    /// Chooses an action.
    ///
    /// # Errors
    ///
    /// A [`CoglingError`] skips this decision; the cogling will then time out.
    fn decide(&mut self, decision: &Decision) -> Result<Value, CoglingError>;

    /// Called when an action was rejected. Return a replacement action to
    /// retry within the remaining time, or `None` to give up on this decision.
    fn on_rejected(
        &mut self,
        _decision: &Decision,
        _code: RejectCode,
        _message: &str,
    ) -> Option<Value> {
        None
    }

    /// Called when a game ends.
    fn on_game_over(&mut self, _result: &GameResult) {}
}

impl<B: Cogling + ?Sized> Cogling for Box<B> {
    fn on_game_start(&mut self, info: &GameInfo) {
        (**self).on_game_start(info);
    }
    fn decide(&mut self, decision: &Decision) -> Result<Value, CoglingError> {
        (**self).decide(decision)
    }
    fn on_rejected(
        &mut self,
        decision: &Decision,
        code: RejectCode,
        message: &str,
    ) -> Option<Value> {
        (**self).on_rejected(decision, code, message)
    }
    fn on_game_over(&mut self, result: &GameResult) {
        (**self).on_game_over(result);
    }
}

/// How to find a game.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Matchmaking {
    /// Quick-match queue.
    Queue {
        /// Game id.
        game: String,
        /// Seat count (game minimum if `None`).
        seats: Option<SeatCount>,
        /// Configuration (canonical default if `None`).
        config: Option<Value>,
    },
    /// Join a specific lobby (plays one game).
    Join(GameId),
}

impl Matchmaking {
    /// Queue for `game` with default seats and configuration.
    pub fn queue(game: impl Into<String>) -> Self {
        Self::Queue {
            game: game.into(),
            seats: None,
            config: None,
        }
    }
}

/// Client settings.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// WebSocket URL, e.g. `ws://127.0.0.1:7878/ws`.
    pub url: String,
    /// Cogling identity.
    pub cogling: CoglingInfo,
    /// API key (hosted mode).
    pub api_key: Option<ApiKey>,
    /// How to find games.
    pub matchmaking: Matchmaking,
    /// Games to play in a row (queue mode only).
    pub games: u32,
}

impl ClientConfig {
    /// Settings for one game.
    pub fn new(url: impl Into<String>, cogling: CoglingInfo, matchmaking: Matchmaking) -> Self {
        Self {
            url: url.into(),
            cogling,
            api_key: None,
            matchmaking,
            games: 1,
        }
    }
}

/// Result of one game from this cogling's point of view.
#[derive(Debug, Clone, PartialEq)]
pub struct GameSummary {
    /// Session id.
    pub game_id: GameId,
    /// Our seat.
    pub seat: Seat,
    /// Final result.
    pub result: GameResult,
}

impl GameSummary {
    /// Whether we finished alone in first place.
    #[must_use]
    pub fn won(&self) -> bool {
        self.result.ranks().is_some_and(|ranks| {
            ranks.get(self.seat.index()).is_some_and(|r| r.get() == 1)
                && ranks.iter().filter(|r| r.get() == 1).count() == 1
        })
    }
}

struct Connection {
    ws: Ws,
}

impl Connection {
    async fn open(config: &ClientConfig) -> Result<Self, ClientError> {
        let (ws, _) = tokio_tungstenite::connect_async(config.url.as_str()).await?;
        let mut conn = Self { ws };
        conn.send(&ClientMessage::Hello {
            protocol: PROTOCOL_VERSION,
            cogling: config.cogling.clone(),
            api_key: config.api_key.clone(),
        })
        .await?;
        match conn.recv().await? {
            ServerMessage::Welcome { .. } => Ok(conn),
            ServerMessage::Error { code, message, .. } => {
                Err(ClientError::Server { code, message })
            }
            other => Err(ClientError::Protocol(format!(
                "expected welcome, got {other:?}"
            ))),
        }
    }

    async fn send(&mut self, msg: &ClientMessage) -> Result<(), ClientError> {
        let text = msg
            .to_json()
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        self.ws.send(Message::text(text)).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<ServerMessage, ClientError> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    return ServerMessage::from_json(text.as_str())
                        .map_err(|e| ClientError::Protocol(e.to_string()));
                }
                Some(Ok(Message::Close(_))) | None => return Err(ClientError::Closed),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
            }
        }
    }
}

/// Creates a lobby game and returns its id (for others to `join`).
///
/// # Errors
///
/// [`ClientError`] if the connection fails or the server rejects the request.
pub async fn create_game(
    config: &ClientConfig,
    request: ClientMessage,
) -> Result<GameId, ClientError> {
    let mut conn = Connection::open(config).await?;
    conn.send(&request).await?;
    let reply = conn.recv().await?;
    // Closing is best effort; the lobby lives on without us.
    let _ = conn.ws.close(None).await;
    match reply {
        ServerMessage::GameCreated { game_id } => Ok(game_id),
        ServerMessage::Error { code, message, .. } => Err(ClientError::Server { code, message }),
        other => Err(ClientError::Protocol(format!(
            "expected game_created, got {other:?}"
        ))),
    }
}

struct InGame {
    info: GameInfo,
    token: ResumeToken,
    pending: Option<Decision>,
}

/// Connects, finds games and plays them with `cogling`.
///
/// # Errors
///
/// [`ClientError`] if the connection fails (and cannot be resumed) or the
/// server reports a blocking error such as an unknown game.
pub async fn run<B: Cogling + ?Sized>(
    config: &ClientConfig,
    cogling: &mut B,
) -> Result<Vec<GameSummary>, ClientError> {
    let target = match config.matchmaking {
        Matchmaking::Join(_) => 1,
        Matchmaking::Queue { .. } => config.games.max(1),
    };
    let mut summaries = Vec::with_capacity(usize::try_from(target).unwrap_or(1));
    let mut conn = Connection::open(config).await?;
    let mut current: Option<InGame> = None;

    while summaries.len() < usize::try_from(target).unwrap_or(usize::MAX) {
        if current.is_none() {
            find_game(&mut conn, config).await?;
        }
        match play(&mut conn, cogling, &mut current, &mut summaries).await {
            Ok(()) => {}
            Err(ClientError::Closed | ClientError::WebSocket(_)) if current.is_some() => {
                conn = resume(config, current.as_ref()).await?;
            }
            Err(e) => return Err(e),
        }
    }
    let _ = conn.ws.close(None).await;
    Ok(summaries)
}

async fn find_game(conn: &mut Connection, config: &ClientConfig) -> Result<(), ClientError> {
    let msg = match &config.matchmaking {
        Matchmaking::Queue {
            game,
            seats,
            config,
        } => ClientMessage::Queue {
            game: game.clone(),
            seats: *seats,
            config: config.clone(),
        },
        Matchmaking::Join(game_id) => ClientMessage::Join { game_id: *game_id },
    };
    conn.send(&msg).await
}

async fn resume(config: &ClientConfig, game: Option<&InGame>) -> Result<Connection, ClientError> {
    let Some(game) = game else {
        return Err(ClientError::Closed);
    };
    let mut delay = Duration::from_millis(100);
    for attempt in 1..=RECONNECT_ATTEMPTS {
        warn!(attempt, game_id = %game.info.game_id, "connection lost; reconnecting");
        tokio::time::sleep(delay).await;
        delay *= 2;
        let Ok(mut conn) = Connection::open(config).await else {
            continue;
        };
        conn.send(&ClientMessage::Reconnect {
            game_id: game.info.game_id,
            resume_token: game.token.clone(),
        })
        .await?;
        return Ok(conn);
    }
    Err(ClientError::Closed)
}

/// Processes messages until a game ends.
async fn play<B: Cogling + ?Sized>(
    conn: &mut Connection,
    cogling: &mut B,
    current: &mut Option<InGame>,
    summaries: &mut Vec<GameSummary>,
) -> Result<(), ClientError> {
    loop {
        match conn.recv().await? {
            ServerMessage::GameStart {
                game_id,
                game,
                config,
                seat,
                players,
                resume_token,
                observation_mode,
                think_time_ms,
                ..
            } => {
                let info = GameInfo {
                    game_id,
                    game,
                    config,
                    seat,
                    seats: players.len(),
                    think_time_ms,
                    observation_mode,
                };
                info!(%game_id, %seat, "game started");
                cogling.on_game_start(&info);
                *current = Some(InGame {
                    info,
                    token: resume_token,
                    pending: None,
                });
            }
            ServerMessage::Decide {
                game_id,
                decision_id,
                remaining_ms,
                observation,
                legal_actions,
            } => {
                let Some(game) = current.as_mut().filter(|g| g.info.game_id == game_id) else {
                    continue;
                };
                let decision = Decision {
                    seat: game.info.seat,
                    decision_id,
                    remaining_ms,
                    observation,
                    legal_actions,
                };
                match cogling.decide(&decision) {
                    Ok(action) => {
                        conn.send(&ClientMessage::Action {
                            game_id,
                            decision_id,
                            action,
                        })
                        .await?;
                    }
                    Err(e) => warn!(error = %e, "cogling could not decide"),
                }
                game.pending = Some(decision);
            }
            ServerMessage::ActionRejected {
                game_id,
                decision_id,
                code,
                message,
                remaining_ms,
            } => {
                debug!(?code, %message, remaining_ms, "action rejected");
                let Some(decision) = current
                    .as_ref()
                    .and_then(|g| g.pending.as_ref())
                    .filter(|d| d.decision_id == decision_id)
                else {
                    continue;
                };
                if let Some(action) = cogling.on_rejected(decision, code, &message) {
                    conn.send(&ClientMessage::Action {
                        game_id,
                        decision_id,
                        action,
                    })
                    .await?;
                }
            }
            ServerMessage::GameOver {
                game_id, result, ..
            } => {
                cogling.on_game_over(&result);
                if let Some(game) = current.take() {
                    info!(%game_id, ?result, "game over");
                    summaries.push(GameSummary {
                        game_id,
                        seat: game.info.seat,
                        result,
                    });
                    return Ok(());
                }
                // A lobby we waited in was cancelled.
                return Err(ClientError::Protocol(format!(
                    "game {game_id} ended before it started: {result:?}"
                )));
            }
            ServerMessage::Error {
                code,
                message,
                fatal,
            } => {
                let blocking = fatal
                    || matches!(
                        code,
                        ErrorCode::UnknownGame
                            | ErrorCode::NoSuchGame
                            | ErrorCode::GameFull
                            | ErrorCode::InvalidConfig
                            | ErrorCode::InvalidResume
                            | ErrorCode::ServerBusy
                    );
                if blocking {
                    return Err(ClientError::Server { code, message });
                }
                warn!(?code, %message, "server reported an error");
            }
            _ => {}
        }
    }
}
