//! Game event logs and deterministic replay.
//!
//! A log is a JSON Lines file: one [`Header`] line followed by ordered
//! [`Event`] lines. The header records everything needed to reproduce the
//! game (game id, exact engine version, ruleset, canonical config, seat
//! count, seed, players). Each accepted action records the resulting state
//! hash, so [`Replayer`] can re-simulate the game and prove it reproduces
//! every intermediate state.
//!
//! Logs are *omniscient*: the seed and full action list reveal hidden
//! information. Serve them only after a game has finished, and use
//! [`Replayer`] to derive public or per-seat views when presenting them.

mod replay;
mod store;

pub use replay::{ReplayError, Replayer};
pub use store::{FileLogStore, LogSink, LogStore, LogWriter, MemoryLogStore, NullLogStore};

use std::io::BufRead;
use std::time::{SystemTime, UNIX_EPOCH};

use ogap_game_core::{GameResult, Seat, SeatCount, Seed, StateHash};
use ogap_protocol::{BotInfo, DecisionId, GameId, ObservationMode, RejectCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Version of the log format written by this crate.
pub const LOG_FORMAT: u32 = 1;

/// Milliseconds since the Unix epoch (0 if the clock is before 1970).
#[must_use]
pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Errors reading or writing logs.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LogError {
    /// Underlying IO failure.
    #[error("log io error: {0}")]
    Io(#[from] std::io::Error),
    /// A line is not valid JSON for its position.
    #[error("line {line}: {source}")]
    Json {
        /// 1-based line number.
        line: usize,
        /// Parse error.
        source: serde_json::Error,
    },
    /// The log is structurally invalid.
    #[error("invalid log: {0}")]
    Format(String),
}

/// Marker for the header line's `"kind": "header"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeaderKind {
    /// The only value.
    #[default]
    Header,
}

/// Where a game was played.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Source {
    /// Headless in-process simulation.
    Sim,
    /// Game server with networked bots.
    Server,
}

/// A seat's occupant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerRecord {
    /// The seat.
    pub seat: Seat,
    /// Bot identity and version.
    pub bot: BotInfo,
}

/// First line of every log: everything needed to reproduce the game.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Header {
    /// Always `"header"`.
    pub kind: HeaderKind,
    /// Log format version ([`LOG_FORMAT`]).
    pub log_format: u32,
    /// Game session id.
    pub game_id: GameId,
    /// Game type id.
    pub game: String,
    /// Exact engine version.
    pub engine_version: String,
    /// Base ruleset id.
    pub ruleset: String,
    /// Canonical configuration.
    pub config: Value,
    /// Seat count.
    pub seats: SeatCount,
    /// Game seed.
    pub seed: Seed,
    /// Seat occupants.
    pub players: Vec<PlayerRecord>,
    /// Where the game was played.
    pub source: Source,
    /// Think time per decision (server games).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think_time_ms: Option<u64>,
    /// Observation mode (server games).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_mode: Option<ObservationMode>,
    /// Wall-clock start time, ms since the Unix epoch.
    pub started_at_ms: u64,
}

/// Per-seat timing and reliability statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatStats {
    /// The seat.
    pub seat: Seat,
    /// Accepted actions.
    pub actions: u32,
    /// Sum of response times of accepted actions.
    pub total_response_ms: u64,
    /// Slowest accepted action.
    pub max_response_ms: u64,
    /// Rejected actions.
    pub rejected: u32,
    /// Decision deadlines missed.
    pub timeouts: u32,
    /// Disconnects.
    pub disconnects: u32,
}

impl SeatStats {
    /// Empty stats for `seat`.
    #[must_use]
    pub const fn new(seat: Seat) -> Self {
        Self {
            seat,
            actions: 0,
            total_response_ms: 0,
            max_response_ms: 0,
            rejected: 0,
            timeouts: 0,
            disconnects: 0,
        }
    }

    /// Records an accepted action.
    pub fn record_action(&mut self, response_ms: u64) {
        self.actions += 1;
        self.total_response_ms += response_ms;
        self.max_response_ms = self.max_response_ms.max(response_ms);
    }
}

/// What happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventBody {
    /// An action was accepted and applied.
    Action {
        /// Acting seat.
        seat: Seat,
        /// Decision answered (server games).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision_id: Option<DecisionId>,
        /// The action payload.
        action: Value,
        /// Time from `decide` to receipt (server games).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_ms: Option<u64>,
        /// Hash of the authoritative state after applying the action.
        state_hash: StateHash,
    },
    /// An action was rejected; state unchanged.
    Rejected {
        /// Acting seat.
        seat: Seat,
        /// Decision answered.
        decision_id: DecisionId,
        /// The rejected payload.
        action: Value,
        /// Why.
        code: RejectCode,
        /// Explanation.
        message: String,
        /// Time since `decide`.
        elapsed_ms: u64,
    },
    /// A decision deadline expired.
    Timeout {
        /// The seat that timed out.
        seat: Seat,
        /// The decision that expired.
        decision_id: DecisionId,
    },
    /// A bot disconnected.
    Disconnect {
        /// The seat.
        seat: Seat,
    },
    /// A bot reconnected.
    Reconnect {
        /// The seat.
        seat: Seat,
    },
    /// The game ended. Always the last event.
    End {
        /// Final result.
        result: GameResult,
        /// Per-seat statistics.
        stats: Vec<SeatStats>,
    },
}

/// A numbered, timestamped event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// 1-based sequence number, strictly increasing.
    pub seq: u64,
    /// Milliseconds since the game started.
    pub t_ms: u64,
    /// What happened.
    #[serde(flatten)]
    pub body: EventBody,
}

/// A complete, validated log.
#[derive(Debug, Clone, PartialEq)]
pub struct GameLog {
    header: Header,
    events: Vec<Event>,
}

impl GameLog {
    /// Validates and builds a log.
    ///
    /// # Errors
    ///
    /// [`LogError::Format`] if the format version is unsupported, sequence
    /// numbers are not `1, 2, 3, ...`, or an `End` event is not last.
    pub fn new(header: Header, events: Vec<Event>) -> Result<Self, LogError> {
        if header.log_format != LOG_FORMAT {
            return Err(LogError::Format(format!(
                "unsupported log format {} (expected {LOG_FORMAT})",
                header.log_format
            )));
        }
        for (expected, event) in (1_u64..).zip(&events) {
            if event.seq != expected {
                return Err(LogError::Format(format!(
                    "expected seq {expected}, found {}",
                    event.seq
                )));
            }
        }
        let end_count = events
            .iter()
            .filter(|e| matches!(e.body, EventBody::End { .. }))
            .count();
        let ends_last = events
            .last()
            .is_some_and(|e| matches!(e.body, EventBody::End { .. }));
        if end_count > 1 || (end_count == 1 && !ends_last) {
            return Err(LogError::Format("end event must be last and unique".into()));
        }
        Ok(Self { header, events })
    }

    /// Reads a JSON Lines log.
    ///
    /// # Errors
    ///
    /// [`LogError`] on IO, parse or validation failure.
    pub fn read(reader: impl BufRead) -> Result<Self, LogError> {
        let mut lines = reader.lines().enumerate();
        let header = loop {
            let Some((i, line)) = lines.next() else {
                return Err(LogError::Format("empty log".into()));
            };
            let line = line?;
            if !line.trim().is_empty() {
                break serde_json::from_str::<Header>(&line).map_err(|source| LogError::Json {
                    line: i + 1,
                    source,
                })?;
            }
        };
        let mut events = Vec::new();
        for (i, line) in lines {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            events.push(
                serde_json::from_str::<Event>(&line).map_err(|source| LogError::Json {
                    line: i + 1,
                    source,
                })?,
            );
        }
        Self::new(header, events)
    }

    /// The header.
    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    /// All events in order.
    #[must_use]
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// The final result, if the game ended.
    #[must_use]
    pub fn result(&self) -> Option<&GameResult> {
        self.events.last().and_then(|e| match &e.body {
            EventBody::End { result, .. } => Some(result),
            _ => None,
        })
    }

    /// Accepted actions in order.
    pub fn actions(&self) -> impl Iterator<Item = (Seat, &Value)> {
        self.events.iter().filter_map(|e| match &e.body {
            EventBody::Action { seat, action, .. } => Some((*seat, action)),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ogap_game_core::{GameRegistry, Status};
    use ogap_tictactoe::TicTacToe;
    use serde_json::json;

    use super::*;

    pub(crate) fn header(config: Value, seed: u64) -> Header {
        Header {
            kind: HeaderKind::Header,
            log_format: LOG_FORMAT,
            game_id: GameId::new(),
            game: "tictactoe".into(),
            engine_version: env!("CARGO_PKG_VERSION").into(),
            ruleset: "standard".into(),
            config,
            seats: SeatCount::TWO,
            seed: Seed::new(seed),
            players: vec![
                PlayerRecord {
                    seat: Seat::FIRST,
                    bot: BotInfo::new("a", Some("1")).unwrap(),
                },
                PlayerRecord {
                    seat: Seat::SECOND,
                    bot: BotInfo::new("b", None).unwrap(),
                },
            ],
            source: Source::Sim,
            think_time_ms: None,
            observation_mode: None,
            started_at_ms: 0,
        }
    }

    pub(crate) fn registry() -> GameRegistry {
        let mut r = GameRegistry::new();
        r.register(TicTacToe).unwrap();
        r
    }

    /// Plays `cells` through the erased API and writes a complete log.
    pub(crate) fn record(cells: &[u8]) -> Vec<u8> {
        let reg = registry();
        let game = reg.get("tictactoe").unwrap();
        let config = game.default_config().unwrap();
        let mut state = game
            .create(Some(&config), SeatCount::TWO, Seed::new(5))
            .unwrap();
        let mut writer = LogWriter::new(Vec::new(), &header(config, 5)).unwrap();
        for (i, cell) in cells.iter().enumerate() {
            let Status::ToAct(active) = state.status().unwrap() else {
                break;
            };
            let seat = active.iter().next().unwrap();
            let action = json!({ "cell": cell });
            state.apply(seat, &action).unwrap();
            writer
                .append(
                    i as u64 * 10,
                    EventBody::Action {
                        seat,
                        decision_id: None,
                        action,
                        response_ms: None,
                        state_hash: state.state_hash().unwrap(),
                    },
                )
                .unwrap();
        }
        if let Status::Finished(outcome) = state.status().unwrap() {
            writer
                .append(
                    999,
                    EventBody::End {
                        result: GameResult::completed(outcome),
                        stats: vec![],
                    },
                )
                .unwrap();
        }
        writer.finish().unwrap()
    }

    #[test]
    fn write_then_read() {
        let bytes = record(&[0, 3, 1, 4, 2]);
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert_eq!(text.lines().count(), 7);
        assert!(text.lines().next().unwrap().contains(r#""kind":"header""#));
        let log = GameLog::read(Cursor::new(bytes)).unwrap();
        assert_eq!(log.events().len(), 6);
        assert_eq!(log.actions().count(), 5);
        assert_eq!(
            log.result().and_then(GameResult::ranks).map(|r| r.len()),
            Some(2)
        );
        let first: Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
        assert_eq!(first["kind"], json!("action"));
        assert_eq!(first["seq"], json!(1));
        assert_eq!(first["action"], json!({"cell": 0}));
        assert_eq!(first["state_hash"].as_str().unwrap().len(), 16);
    }

    #[test]
    fn rejects_bad_structure() {
        assert!(matches!(
            GameLog::read(Cursor::new("")),
            Err(LogError::Format(_))
        ));
        assert!(matches!(
            GameLog::read(Cursor::new("{}\n")),
            Err(LogError::Json { line: 1, .. })
        ));

        let bytes = record(&[0, 3, 1, 4, 2]);
        let text = String::from_utf8(bytes).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.swap(1, 2);
        let err = GameLog::read(Cursor::new(lines.join("\n"))).unwrap_err();
        assert!(err.to_string().contains("expected seq 1"), "{err}");

        let mut h = header(json!({}), 0);
        h.log_format = 99;
        assert!(GameLog::new(h, vec![]).is_err());

        let end = |seq| Event {
            seq,
            t_ms: 0,
            body: EventBody::End {
                result: GameResult::Aborted { reason: "x".into() },
                stats: vec![],
            },
        };
        let disc = Event {
            seq: 2,
            t_ms: 0,
            body: EventBody::Disconnect { seat: Seat::FIRST },
        };
        assert!(GameLog::new(header(json!({}), 0), vec![end(1), disc]).is_err());
        assert!(GameLog::new(header(json!({}), 0), vec![end(1)]).is_ok());
    }

    #[test]
    fn seat_stats_accumulate() {
        let mut s = SeatStats::new(Seat::FIRST);
        s.record_action(10);
        s.record_action(30);
        assert_eq!(
            (s.actions, s.total_response_ms, s.max_response_ms),
            (2, 40, 30)
        );
    }
}
