//! The bot communication protocol, version [`PROTOCOL_VERSION`].
//!
//! Bots talk to the server over a WebSocket using JSON text frames. Every
//! frame is one message object with a `"type"` discriminator. See
//! `docs/protocol.md` for the full specification and lifecycle, and
//! [`json_schema`] for a machine-readable schema.
//!
//! Unknown fields are ignored so that newer clients can talk to older
//! servers within the same protocol version; unknown message types are
//! rejected.

mod ids;

pub use ids::{
    ApiKey, BotInfo, BotInfoError, BotName, BotVersion, DecisionId, GameId, ResumeToken,
};
pub use ogap_game_core::{ForfeitReason, GameResult, Rank, Seat, SeatCount};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// The protocol version implemented by this crate.
pub const PROTOCOL_VERSION: u32 = 1;

/// What each `decide` message contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObservationMode {
    /// The player-specific observation only.
    State,
    /// The observation plus the list of currently legal actions.
    #[default]
    StateAndLegal,
}

/// One seat at the table, as announced in `game_start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PlayerSlot {
    /// The seat.
    pub seat: Seat,
    /// The bot occupying it.
    pub bot: BotInfo,
}

/// Server limits announced in `welcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Limits {
    /// Largest accepted frame, in bytes.
    pub max_message_bytes: u32,
    /// Sustained message rate allowed per connection.
    pub max_messages_per_second: u32,
}

/// Messages sent by bots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClientMessage {
    /// First message on every connection.
    Hello {
        /// Protocol version the bot speaks; must equal [`PROTOCOL_VERSION`].
        protocol: u32,
        /// Self-declared identity.
        bot: BotInfo,
        /// Developer API key; required in hosted mode, ignored in standalone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<ApiKey>,
    },
    /// Creates a lobby game that bots can `join` by id.
    CreateGame {
        /// Game id, e.g. `"connect4"`.
        game: String,
        /// Seat count; defaults to the game's minimum.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seats: Option<SeatCount>,
        /// Game configuration; defaults to the canonical ruleset.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<Value>,
        /// Think time per decision; defaults to the server setting.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        think_time_ms: Option<u64>,
        /// Observation mode; defaults to the server setting.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observation_mode: Option<ObservationMode>,
    },
    /// Takes a seat in a lobby game.
    Join {
        /// The game to join.
        game_id: GameId,
    },
    /// Enters the quick-match queue for a game.
    Queue {
        /// Game id, e.g. `"tictactoe"`.
        game: String,
        /// Seat count; defaults to the game's minimum.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seats: Option<SeatCount>,
        /// Game configuration; only bots asking for the same configuration
        /// are matched together.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<Value>,
    },
    /// Leaves the quick-match queue.
    LeaveQueue,
    /// Responds to a `decide`.
    Action {
        /// The game.
        game_id: GameId,
        /// Must match the pending decision.
        decision_id: DecisionId,
        /// Game-specific action payload.
        action: Value,
    },
    /// Reclaims a seat after a disconnect.
    Reconnect {
        /// The game.
        game_id: GameId,
        /// Token received in `game_start`.
        resume_token: ResumeToken,
    },
    /// Liveness check.
    Ping {
        /// Echoed back in `pong`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nonce: Option<u64>,
    },
}

/// Why an action was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RejectCode {
    /// The action violates the game rules.
    IllegalAction,
    /// The action payload does not match the game's action format.
    MalformedAction,
    /// It is not this seat's turn.
    NotYourTurn,
    /// `decision_id` does not match the pending decision.
    StaleDecision,
    /// The game is over.
    GameFinished,
}

/// Connection-level error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
    /// The bot's protocol version is not supported.
    ProtocolVersion,
    /// The frame is not a valid message.
    Malformed,
    /// The message is valid but not allowed in the current state.
    UnexpectedMessage,
    /// `hello` must be the first message.
    HelloRequired,
    /// Authentication is required or failed.
    AuthRequired,
    /// No game type with that id exists.
    UnknownGame,
    /// No game session with that id exists.
    NoSuchGame,
    /// The lobby is full or already started.
    GameFull,
    /// The configuration, seat count or settings were rejected.
    InvalidConfig,
    /// The resume token does not match any seat in the game.
    InvalidResume,
    /// The seat was taken over by a newer connection.
    Superseded,
    /// Too many messages.
    RateLimited,
    /// Frame larger than the advertised limit.
    MessageTooLarge,
    /// The server is at capacity.
    ServerBusy,
    /// Something went wrong on the server.
    Internal,
}

/// Messages sent by the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ServerMessage {
    /// Reply to `hello`.
    Welcome {
        /// Protocol version spoken by the server.
        protocol: u32,
        /// Server software version.
        server_version: String,
        /// Server limits.
        limits: Limits,
    },
    /// Reply to `create_game`.
    GameCreated {
        /// Id to `join`.
        game_id: GameId,
    },
    /// Seat taken in a lobby that is still filling up.
    Joined {
        /// The game.
        game_id: GameId,
        /// Seats filled so far.
        seats_filled: u8,
        /// Seats needed to start.
        seats_total: u8,
    },
    /// Waiting in the quick-match queue.
    Queued {
        /// Game id.
        game: String,
    },
    /// The game has started (also re-sent after a successful reconnect).
    GameStart {
        /// The game.
        game_id: GameId,
        /// Game type id.
        game: String,
        /// Exact engine version.
        engine_version: String,
        /// Base ruleset id.
        ruleset: String,
        /// Canonical configuration in effect.
        config: Value,
        /// Your seat.
        seat: Seat,
        /// Everyone at the table.
        players: Vec<PlayerSlot>,
        /// Secret for `reconnect`.
        resume_token: ResumeToken,
        /// What `decide` messages contain.
        observation_mode: ObservationMode,
        /// Think time per decision.
        think_time_ms: u64,
    },
    /// Your turn: respond with exactly one `action` before the deadline.
    Decide {
        /// The game.
        game_id: GameId,
        /// Echo this in your action.
        decision_id: DecisionId,
        /// Milliseconds left on the decision clock, measured by the server.
        remaining_ms: u64,
        /// Your complete, self-contained observation.
        observation: Value,
        /// Legal actions (only in `state_and_legal` mode).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        legal_actions: Option<Vec<Value>>,
    },
    /// The action was applied.
    ActionAccepted {
        /// The game.
        game_id: GameId,
        /// The decision it answered.
        decision_id: DecisionId,
    },
    /// The action was rejected; you may retry within the remaining time.
    ActionRejected {
        /// The game.
        game_id: GameId,
        /// The decision it answered.
        decision_id: DecisionId,
        /// Machine-readable reason.
        code: RejectCode,
        /// Human-readable explanation.
        message: String,
        /// Milliseconds left on the (unchanged) decision clock.
        remaining_ms: u64,
    },
    /// The game is over.
    GameOver {
        /// The game.
        game_id: GameId,
        /// Final result.
        result: GameResult,
        /// Your final observation.
        observation: Value,
    },
    /// A connection-level error. If `fatal`, the server closes the socket.
    Error {
        /// Machine-readable code.
        code: ErrorCode,
        /// Human-readable explanation.
        message: String,
        /// Whether the connection will be closed.
        fatal: bool,
    },
    /// Reply to `ping`.
    Pong {
        /// The nonce from `ping`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nonce: Option<u64>,
    },
}

/// A frame could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("malformed message: {0}")]
pub struct DecodeError(String);

impl ClientMessage {
    /// Parses a client frame.
    ///
    /// # Errors
    ///
    /// [`DecodeError`] if the text is not a valid client message.
    pub fn from_json(text: &str) -> Result<Self, DecodeError> {
        serde_json::from_str(text).map_err(|e| DecodeError(e.to_string()))
    }

    /// Serializes to a JSON frame.
    ///
    /// # Errors
    ///
    /// [`DecodeError`] if serialization fails (only possible for exotic
    /// action payloads).
    pub fn to_json(&self) -> Result<String, DecodeError> {
        serde_json::to_string(self).map_err(|e| DecodeError(e.to_string()))
    }
}

impl ServerMessage {
    /// Parses a server frame.
    ///
    /// # Errors
    ///
    /// [`DecodeError`] if the text is not a valid server message.
    pub fn from_json(text: &str) -> Result<Self, DecodeError> {
        serde_json::from_str(text).map_err(|e| DecodeError(e.to_string()))
    }

    /// Serializes to a JSON frame.
    ///
    /// # Errors
    ///
    /// [`DecodeError`] if serialization fails.
    pub fn to_json(&self) -> Result<String, DecodeError> {
        serde_json::to_string(self).map_err(|e| DecodeError(e.to_string()))
    }

    /// Shorthand for an error message.
    pub fn error(code: ErrorCode, message: impl Into<String>, fatal: bool) -> Self {
        Self::Error {
            code,
            message: message.into(),
            fatal,
        }
    }
}

/// JSON Schema for both directions of the protocol.
///
/// # Examples
///
/// ```
/// let schema = ogap_protocol::json_schema();
/// assert!(schema["client_message"].is_object());
/// assert!(schema["server_message"].is_object());
/// ```
#[must_use]
pub fn json_schema() -> Value {
    serde_json::json!({
        "protocol_version": PROTOCOL_VERSION,
        "client_message": schemars::schema_for!(ClientMessage),
        "server_message": schemars::schema_for!(ServerMessage),
    })
}

#[cfg(test)]
mod tests {
    use ogap_game_core::{ForfeitPolicy, Seat};
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    fn game_id() -> GameId {
        "0192f6a0-0000-7000-8000-000000000001".parse().unwrap()
    }

    fn bot() -> BotInfo {
        BotInfo::new("Strategist", Some("2.3.1")).unwrap()
    }

    fn client_examples() -> Vec<ClientMessage> {
        vec![
            ClientMessage::Hello {
                protocol: PROTOCOL_VERSION,
                bot: bot(),
                api_key: None,
            },
            ClientMessage::CreateGame {
                game: "connect4".into(),
                seats: Some(SeatCount::TWO),
                config: Some(json!({"first_seat": 1})),
                think_time_ms: Some(500),
                observation_mode: Some(ObservationMode::State),
            },
            ClientMessage::Join { game_id: game_id() },
            ClientMessage::Queue {
                game: "tictactoe".into(),
                seats: None,
                config: None,
            },
            ClientMessage::LeaveQueue,
            ClientMessage::Action {
                game_id: game_id(),
                decision_id: DecisionId::new(7),
                action: json!({"cell": 4}),
            },
            ClientMessage::Reconnect {
                game_id: game_id(),
                resume_token: ResumeToken::new("s3cret"),
            },
            ClientMessage::Ping { nonce: Some(1) },
        ]
    }

    fn server_examples() -> Vec<ServerMessage> {
        vec![
            ServerMessage::Welcome {
                protocol: PROTOCOL_VERSION,
                server_version: "0.1.0".into(),
                limits: Limits {
                    max_message_bytes: 65_536,
                    max_messages_per_second: 50,
                },
            },
            ServerMessage::GameCreated { game_id: game_id() },
            ServerMessage::Joined {
                game_id: game_id(),
                seats_filled: 1,
                seats_total: 2,
            },
            ServerMessage::Queued {
                game: "tictactoe".into(),
            },
            ServerMessage::GameStart {
                game_id: game_id(),
                game: "tictactoe".into(),
                engine_version: "0.1.0".into(),
                ruleset: "standard".into(),
                config: json!({"first_seat": 0}),
                seat: Seat::FIRST,
                players: vec![
                    PlayerSlot {
                        seat: Seat::FIRST,
                        bot: bot(),
                    },
                    PlayerSlot {
                        seat: Seat::SECOND,
                        bot: BotInfo::new("random", None).unwrap(),
                    },
                ],
                resume_token: ResumeToken::new("s3cret"),
                observation_mode: ObservationMode::StateAndLegal,
                think_time_ms: 1000,
            },
            ServerMessage::Decide {
                game_id: game_id(),
                decision_id: DecisionId::FIRST,
                remaining_ms: 1000,
                observation: json!({"board": [null, null, null, null, null, null, null, null, null], "seat": 0, "mark": "X", "to_move": 0}),
                legal_actions: Some(vec![json!({"cell": 0}), json!({"cell": 1})]),
            },
            ServerMessage::ActionAccepted {
                game_id: game_id(),
                decision_id: DecisionId::FIRST,
            },
            ServerMessage::ActionRejected {
                game_id: game_id(),
                decision_id: DecisionId::FIRST,
                code: RejectCode::IllegalAction,
                message: "cell 4 is already taken by X".into(),
                remaining_ms: 640,
            },
            ServerMessage::GameOver {
                game_id: game_id(),
                result: GameResult::forfeit(
                    Seat::SECOND,
                    ForfeitReason::Timeout,
                    SeatCount::TWO,
                    ForfeitPolicy::TiedOthers,
                    None,
                ),
                observation: json!({}),
            },
            ServerMessage::error(ErrorCode::Malformed, "expected value at line 1", false),
            ServerMessage::Pong { nonce: Some(1) },
        ]
    }

    #[test]
    fn client_messages_wire_format() {
        let frames: Vec<Value> = client_examples()
            .iter()
            .map(|m| serde_json::to_value(m).unwrap())
            .collect();
        insta::assert_json_snapshot!(frames);
    }

    #[test]
    fn server_messages_wire_format() {
        let frames: Vec<Value> = server_examples()
            .iter()
            .map(|m| serde_json::to_value(m).unwrap())
            .collect();
        insta::assert_json_snapshot!(frames);
    }

    #[test]
    fn messages_round_trip() {
        for msg in client_examples() {
            let text = msg.to_json().unwrap();
            assert_eq!(ClientMessage::from_json(&text).unwrap(), msg);
        }
        for msg in server_examples() {
            let text = msg.to_json().unwrap();
            assert_eq!(ServerMessage::from_json(&text).unwrap(), msg);
        }
    }

    #[test]
    fn decoding_errors() {
        assert!(ClientMessage::from_json("not json").is_err());
        assert!(ClientMessage::from_json(r#"{"type":"teleport"}"#).is_err());
        assert!(ClientMessage::from_json(r#"{"type":"join"}"#).is_err());
        assert!(ClientMessage::from_json(r#"{"type":"join","game_id":"x"}"#).is_err());
        // Invalid bot identity is rejected at parse time.
        assert!(
            ClientMessage::from_json(r#"{"type":"hello","protocol":1,"bot":{"name":""}}"#).is_err()
        );
        // Unknown fields are ignored for forward compatibility.
        let msg = ClientMessage::from_json(r#"{"type":"ping","extra":true}"#).unwrap();
        assert_eq!(msg, ClientMessage::Ping { nonce: None });
    }

    #[test]
    fn schema_is_up_to_date() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/protocol.schema.json"
        );
        let generated = format!(
            "{}\n",
            serde_json::to_string_pretty(&json_schema()).unwrap()
        );
        if std::env::var_os("UPDATE_SCHEMA").is_some() {
            std::fs::write(path, &generated).unwrap();
        }
        let on_disk = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            on_disk == generated,
            "docs/protocol.schema.json is stale; rerun with UPDATE_SCHEMA=1"
        );
    }

    proptest! {
        /// Arbitrary input never panics the decoder.
        #[test]
        fn decoder_never_panics(text in ".{0,200}") {
            let _ = ClientMessage::from_json(&text);
        }

        /// Arbitrary JSON objects with a known type never panic either.
        #[test]
        fn decoder_handles_odd_fields(ty in "(hello|join|action|queue|ping|reconnect)", n in any::<i64>(), s in ".{0,20}") {
            let text = json!({"type": ty, "protocol": n, "game_id": s, "decision_id": n, "action": s, "game": s}).to_string();
            let _ = ClientMessage::from_json(&text);
        }
    }
}
