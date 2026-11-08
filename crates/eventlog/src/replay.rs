//! Deterministic re-simulation of logged games.

use ogap_game_core::{DynError, DynState, GameRegistry, GameResult, StateHash, Status};
use thiserror::Error;

use crate::{Event, EventBody, GameLog};

/// Replay failed or diverged.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReplayError {
    /// The log's game is not registered.
    #[error("game {0:?} is not available")]
    UnknownGame(String),
    /// The registered engine differs from the one that recorded the log.
    #[error("log was recorded with {game} engine {recorded}, but {available} is installed")]
    EngineVersion {
        /// Game id.
        game: String,
        /// Version in the log.
        recorded: String,
        /// Version available.
        available: String,
    },
    /// The registered engine implements a different ruleset.
    #[error("log uses ruleset {recorded:?}, engine implements {available:?}")]
    Ruleset {
        /// Ruleset in the log.
        recorded: String,
        /// Ruleset available.
        available: String,
    },
    /// The recorded setup was rejected.
    #[error("cannot recreate game: {0}")]
    Setup(DynError),
    /// A recorded action was rejected on replay.
    #[error("event {seq}: recorded action rejected on replay: {source}")]
    Apply {
        /// Sequence number.
        seq: u64,
        /// Engine error.
        source: DynError,
    },
    /// The replayed state differs from the recorded one.
    #[error("event {seq}: state hash {actual} differs from recorded {recorded}")]
    Diverged {
        /// Sequence number.
        seq: u64,
        /// Hash in the log.
        recorded: StateHash,
        /// Hash on replay.
        actual: StateHash,
    },
    /// The recorded completed result differs from the engine's outcome.
    #[error("recorded result does not match the replayed outcome")]
    OutcomeMismatch,
    /// The engine failed to serialize something.
    #[error("engine error: {0}")]
    Engine(DynError),
}

/// Steps through a log, re-applying actions and checking state hashes.
///
/// At any point the current authoritative state is available through
/// [`Replayer::state`], from which public or per-seat views can be derived.
/// This is the basis for spectator playback (any speed, pause, step),
/// debugging and analysis.
pub struct Replayer<'a> {
    log: &'a GameLog,
    state: Box<dyn DynState>,
    position: usize,
}

impl<'a> Replayer<'a> {
    /// Recreates the initial state of `log`'s game.
    ///
    /// # Errors
    ///
    /// [`ReplayError`] if the game is unavailable, the engine version or
    /// ruleset differ, or the setup is rejected.
    pub fn new(registry: &GameRegistry, log: &'a GameLog) -> Result<Self, ReplayError> {
        let header = log.header();
        let game = registry
            .get(&header.game)
            .ok_or_else(|| ReplayError::UnknownGame(header.game.clone()))?;
        let info = game.info();
        if info.engine_version != header.engine_version {
            return Err(ReplayError::EngineVersion {
                game: header.game.clone(),
                recorded: header.engine_version.clone(),
                available: info.engine_version.to_owned(),
            });
        }
        if info.ruleset != header.ruleset {
            return Err(ReplayError::Ruleset {
                recorded: header.ruleset.clone(),
                available: info.ruleset.to_owned(),
            });
        }
        let state = game
            .create(Some(&header.config), header.seats, header.seed)
            .map_err(ReplayError::Setup)?;
        Ok(Self {
            log,
            state,
            position: 0,
        })
    }

    /// The current authoritative state.
    #[must_use]
    pub fn state(&self) -> &dyn DynState {
        self.state.as_ref()
    }

    /// The last event processed, if any.
    #[must_use]
    pub fn current_event(&self) -> Option<&'a Event> {
        self.position
            .checked_sub(1)
            .and_then(|i| self.log.events().get(i))
    }

    /// Processes the next event and returns it, or `None` at the end.
    ///
    /// Only `action` events change the state; each is verified against its
    /// recorded hash.
    ///
    /// # Errors
    ///
    /// [`ReplayError::Apply`] or [`ReplayError::Diverged`] if the replay
    /// does not reproduce the log, [`ReplayError::OutcomeMismatch`] if the
    /// recorded completed result differs from the engine's.
    pub fn step(&mut self) -> Result<Option<&'a Event>, ReplayError> {
        let Some(event) = self.log.events().get(self.position) else {
            return Ok(None);
        };
        match &event.body {
            EventBody::Action {
                seat,
                action,
                state_hash,
                ..
            } => {
                self.state
                    .apply(*seat, action)
                    .map_err(|source| ReplayError::Apply {
                        seq: event.seq,
                        source,
                    })?;
                let actual = self.state.state_hash().map_err(ReplayError::Engine)?;
                if actual != *state_hash {
                    return Err(ReplayError::Diverged {
                        seq: event.seq,
                        recorded: *state_hash,
                        actual,
                    });
                }
            }
            EventBody::End {
                result: recorded @ GameResult::Completed { .. },
                ..
            } => {
                let Status::Finished(outcome) = self.state.status().map_err(ReplayError::Engine)?
                else {
                    return Err(ReplayError::OutcomeMismatch);
                };
                if GameResult::completed(outcome) != *recorded {
                    return Err(ReplayError::OutcomeMismatch);
                }
            }
            _ => {}
        }
        self.position += 1;
        Ok(Some(event))
    }

    /// Replays every remaining event.
    ///
    /// # Errors
    ///
    /// See [`Replayer::step`].
    pub fn run_to_end(&mut self) -> Result<(), ReplayError> {
        while self.step()?.is_some() {}
        Ok(())
    }

    /// Advances until the event with sequence number `seq` has been processed
    /// (or the log ends).
    ///
    /// # Errors
    ///
    /// See [`Replayer::step`].
    pub fn seek(&mut self, seq: u64) -> Result<(), ReplayError> {
        while self.current_event().is_none_or(|e| e.seq < seq) {
            if self.step()?.is_none() {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ogap_game_core::Seat;
    use serde_json::{Value, json};

    use super::*;
    use crate::tests::{record, registry};

    fn log(cells: &[u8]) -> GameLog {
        GameLog::read(Cursor::new(record(cells))).unwrap()
    }

    fn mutate(bytes: Vec<u8>, f: impl Fn(&mut Vec<Value>)) -> GameLog {
        let mut lines: Vec<Value> = String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        f(&mut lines);
        let text: Vec<String> = lines.iter().map(Value::to_string).collect();
        GameLog::read(Cursor::new(text.join("\n"))).unwrap()
    }

    #[test]
    fn replays_and_verifies_a_complete_game() {
        let log = log(&[0, 3, 1, 4, 2]);
        let reg = registry();
        let mut r = Replayer::new(&reg, &log).unwrap();
        assert!(r.current_event().is_none());
        r.run_to_end().unwrap();
        assert!(r.state().status().unwrap().is_finished());
        assert!(r.step().unwrap().is_none());
    }

    #[test]
    fn seek_and_views() {
        let log = log(&[0, 3, 1, 4, 2]);
        let reg = registry();
        let mut r = Replayer::new(&reg, &log).unwrap();
        r.seek(2).unwrap();
        assert_eq!(r.current_event().unwrap().seq, 2);
        let view = r.state().public_view().unwrap();
        assert_eq!(view["board"][0], json!("X"));
        assert_eq!(view["board"][3], json!("O"));
        assert_eq!(view["board"][1], json!(null));
        let seat_view = r.state().player_view(Seat::SECOND).unwrap();
        assert_eq!(seat_view["mark"], json!("O"));
        r.seek(1_000).unwrap();
        assert_eq!(r.current_event().unwrap().seq, 6);
    }

    #[test]
    fn detects_divergence() {
        let bytes = record(&[0, 3, 1, 4, 2]);
        let tampered = mutate(bytes.clone(), |lines| {
            lines[2]["state_hash"] = json!("0000000000000000");
        });
        let reg = registry();
        let mut r = Replayer::new(&reg, &tampered).unwrap();
        assert!(matches!(
            r.run_to_end(),
            Err(ReplayError::Diverged { seq: 2, .. })
        ));

        let illegal = mutate(bytes.clone(), |lines| {
            lines[2]["action"] = json!({"cell": 0});
        });
        let mut r = Replayer::new(&reg, &illegal).unwrap();
        assert!(matches!(
            r.run_to_end(),
            Err(ReplayError::Apply { seq: 2, .. })
        ));

        let wrong_result = mutate(bytes, |lines| {
            lines[6]["result"]["ranks"] = json!([2, 1]);
        });
        let mut r = Replayer::new(&reg, &wrong_result).unwrap();
        assert!(matches!(r.run_to_end(), Err(ReplayError::OutcomeMismatch)));
    }

    #[test]
    fn refuses_mismatched_engines() {
        let bytes = record(&[4]);
        let reg = registry();
        let other_version = mutate(bytes.clone(), |l| l[0]["engine_version"] = json!("9.9.9"));
        assert!(matches!(
            Replayer::new(&reg, &other_version),
            Err(ReplayError::EngineVersion { .. })
        ));
        let other_rules = mutate(bytes.clone(), |l| l[0]["ruleset"] = json!("misere"));
        assert!(matches!(
            Replayer::new(&reg, &other_rules),
            Err(ReplayError::Ruleset { .. })
        ));
        let unknown = mutate(bytes.clone(), |l| l[0]["game"] = json!("chess"));
        assert!(matches!(
            Replayer::new(&reg, &unknown),
            Err(ReplayError::UnknownGame(_))
        ));
        let bad_config = mutate(bytes, |l| l[0]["config"] = json!({"first_seat": 7}));
        assert!(matches!(
            Replayer::new(&reg, &bad_config),
            Err(ReplayError::Setup(_))
        ));
    }
}
