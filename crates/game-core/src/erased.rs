//! Type-erased, JSON-facing view of the [`Game`] contract.
//!
//! Servers, protocols and replay tools deal with many games chosen at runtime.
//! They use [`DynGame`] / [`DynState`], which every [`Game`] gets for free via
//! [`GameRegistry::register`]. High-volume simulation can keep using the typed
//! API directly.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize as _, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::game::{ConfigError, Game, GameInfo, IllegalAction, Setup};
use crate::hash::StateHash;
use crate::outcome::{Rank, Status};
use crate::rng::Seed;
use crate::seat::{Seat, SeatCount};

/// Errors from the type-erased API.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum DynError {
    /// The configuration was rejected.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The action JSON does not match the game's action format.
    #[error("malformed action: {0}")]
    MalformedAction(String),
    /// The action is well-formed but illegal.
    #[error(transparent)]
    Illegal(#[from] IllegalAction),
    /// The seat does not exist in this game.
    #[error("{seat} does not exist in a {seats}-seat game")]
    NoSuchSeat {
        /// Offending seat.
        seat: Seat,
        /// Seats in the game.
        seats: SeatCount,
    },
    /// An engine value could not be serialized (an engine bug).
    #[error("serialization failed: {0}")]
    Serialization(String),
}

impl DynError {
    /// Stable machine-readable code, used in the cogling protocol.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Config(_) => "invalid_config",
            Self::MalformedAction(_) => "malformed_action",
            Self::Illegal(i) => i.code(),
            Self::NoSuchSeat { .. } => "no_such_seat",
            Self::Serialization(_) => "internal_error",
        }
    }
}

fn to_json<T: Serialize + ?Sized>(value: &T) -> Result<Value, DynError> {
    serde_json::to_value(value).map_err(|e| DynError::Serialization(e.to_string()))
}

/// A game chosen at runtime.
pub trait DynGame: Send + Sync {
    /// Static metadata.
    fn info(&self) -> &GameInfo;

    /// The canonical default configuration as JSON.
    ///
    /// # Errors
    ///
    /// [`DynError::Serialization`] if the config type cannot be serialized.
    fn default_config(&self) -> Result<Value, DynError>;

    /// Parses and validates a configuration, returning its canonical JSON
    /// form (all defaults filled in). `None` means the default configuration.
    ///
    /// # Errors
    ///
    /// [`DynError::Config`] if the configuration is malformed or invalid.
    fn normalize_config(&self, config: Option<&Value>, seats: SeatCount)
    -> Result<Value, DynError>;

    /// Creates a new game.
    ///
    /// # Errors
    ///
    /// [`DynError::Config`] if the configuration is malformed or invalid.
    fn create(
        &self,
        config: Option<&Value>,
        seats: SeatCount,
        seed: Seed,
    ) -> Result<Box<dyn DynState>, DynError>;
}

/// A running game chosen at runtime. Owns its authoritative state.
pub trait DynState: Send + Sync {
    /// Static metadata of the game being played.
    fn info(&self) -> &GameInfo;
    /// Canonical configuration JSON.
    fn config(&self) -> &Value;
    /// Seat count.
    fn seats(&self) -> SeatCount;
    /// Seed the game was created with.
    fn seed(&self) -> Seed;

    /// Who must act, or the outcome with JSON scores.
    ///
    /// # Errors
    ///
    /// [`DynError::Serialization`] if scores cannot be serialized.
    fn status(&self) -> Result<Status<Value>, DynError>;

    /// Legal actions for `seat` as JSON.
    ///
    /// # Errors
    ///
    /// [`DynError::NoSuchSeat`] or [`DynError::Serialization`].
    fn legal_actions(&self, seat: Seat) -> Result<Vec<Value>, DynError>;

    /// Parses and applies an action. State is unchanged on error.
    ///
    /// # Errors
    ///
    /// [`DynError::MalformedAction`], [`DynError::Illegal`] or [`DynError::NoSuchSeat`].
    fn apply(&mut self, seat: Seat, action: &Value) -> Result<(), DynError>;

    /// The observation for `seat`.
    ///
    /// # Errors
    ///
    /// [`DynError::NoSuchSeat`] or [`DynError::Serialization`].
    fn player_view(&self, seat: Seat) -> Result<Value, DynError>;

    /// The spectator observation.
    ///
    /// # Errors
    ///
    /// [`DynError::Serialization`].
    fn public_view(&self) -> Result<Value, DynError>;

    /// The full authoritative state (omniscient; never send to coglings mid-game).
    ///
    /// # Errors
    ///
    /// [`DynError::Serialization`].
    fn state_json(&self) -> Result<Value, DynError>;

    /// Hash of the authoritative state.
    ///
    /// # Errors
    ///
    /// [`DynError::Serialization`].
    fn state_hash(&self) -> Result<StateHash, DynError>;

    /// The engine's provisional standing, if any.
    fn provisional_ranks(&self) -> Option<Vec<Rank>>;
}

struct ErasedGame<G>(Arc<G>);

struct ErasedState<G: Game> {
    game: Arc<G>,
    config_json: Value,
    setup: Setup<G::Config>,
    state: G::State,
}

impl<G: Game> ErasedGame<G> {
    fn parse_setup(
        &self,
        config: Option<&Value>,
        seats: SeatCount,
        seed: Seed,
    ) -> Result<(Setup<G::Config>, Value), DynError> {
        let config: G::Config = match config {
            None | Some(Value::Null) => G::Config::default(),
            Some(v) => {
                G::Config::deserialize(v).map_err(|e| ConfigError::Malformed(e.to_string()))?
            }
        };
        let json = to_json(&config)?;
        Ok((Setup::new(self.0.as_ref(), config, seats, seed)?, json))
    }
}

impl<G: Game> DynGame for ErasedGame<G> {
    fn info(&self) -> &GameInfo {
        self.0.info()
    }

    fn default_config(&self) -> Result<Value, DynError> {
        to_json(&G::Config::default())
    }

    fn normalize_config(
        &self,
        config: Option<&Value>,
        seats: SeatCount,
    ) -> Result<Value, DynError> {
        self.parse_setup(config, seats, Seed::default())
            .map(|(_, json)| json)
    }

    fn create(
        &self,
        config: Option<&Value>,
        seats: SeatCount,
        seed: Seed,
    ) -> Result<Box<dyn DynState>, DynError> {
        let (setup, config_json) = self.parse_setup(config, seats, seed)?;
        let state = self.0.new_game(&setup);
        Ok(Box::new(ErasedState {
            game: Arc::clone(&self.0),
            config_json,
            setup,
            state,
        }))
    }
}

impl<G: Game> ErasedState<G> {
    fn check_seat(&self, seat: Seat) -> Result<(), DynError> {
        let seats = self.setup.seats();
        if seats.contains(seat) {
            Ok(())
        } else {
            Err(DynError::NoSuchSeat { seat, seats })
        }
    }
}

impl<G: Game> DynState for ErasedState<G> {
    fn info(&self) -> &GameInfo {
        self.game.info()
    }

    fn config(&self) -> &Value {
        &self.config_json
    }

    fn seats(&self) -> SeatCount {
        self.setup.seats()
    }

    fn seed(&self) -> Seed {
        self.setup.seed()
    }

    fn status(&self) -> Result<Status<Value>, DynError> {
        match self.game.status(&self.state) {
            Status::ToAct(seats) => Ok(Status::ToAct(seats)),
            Status::Finished(outcome) => outcome
                .try_map_scores(|s| to_json(&s))
                .map(Status::Finished),
        }
    }

    fn legal_actions(&self, seat: Seat) -> Result<Vec<Value>, DynError> {
        self.check_seat(seat)?;
        self.game
            .legal_actions(&self.state, seat)
            .iter()
            .map(to_json)
            .collect()
    }

    fn apply(&mut self, seat: Seat, action: &Value) -> Result<(), DynError> {
        self.check_seat(seat)?;
        let action =
            G::Action::deserialize(action).map_err(|e| DynError::MalformedAction(e.to_string()))?;
        self.game.apply(&mut self.state, seat, &action)?;
        Ok(())
    }

    fn player_view(&self, seat: Seat) -> Result<Value, DynError> {
        self.check_seat(seat)?;
        to_json(&self.game.player_view(&self.state, seat))
    }

    fn public_view(&self) -> Result<Value, DynError> {
        to_json(&self.game.public_view(&self.state))
    }

    fn state_json(&self) -> Result<Value, DynError> {
        to_json(&self.state)
    }

    fn state_hash(&self) -> Result<StateHash, DynError> {
        StateHash::of(&self.state).map_err(|e| DynError::Serialization(e.to_string()))
    }

    fn provisional_ranks(&self) -> Option<Vec<Rank>> {
        self.game.provisional_ranks(&self.state)
    }
}

/// Error registering a game.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// A game with the same id is already registered.
    #[error("game {0:?} is already registered")]
    Duplicate(&'static str),
}

/// The set of games available at runtime, keyed by [`GameInfo::id`].
///
/// Engines are registered once at startup; lookups are cheap `Arc` clones.
#[derive(Clone, Default)]
pub struct GameRegistry {
    games: BTreeMap<&'static str, Arc<dyn DynGame>>,
}

impl GameRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a game.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Duplicate`] if the id is taken.
    pub fn register<G: Game>(&mut self, game: G) -> Result<(), RegistryError> {
        let id = game.info().id;
        if self.games.contains_key(id) {
            return Err(RegistryError::Duplicate(id));
        }
        self.games.insert(id, Arc::new(ErasedGame(Arc::new(game))));
        Ok(())
    }

    /// Looks up a game by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Arc<dyn DynGame>> {
        self.games.get(id)
    }

    /// Iterates over registered games in id order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn DynGame>> {
        self.games.values()
    }
}

impl std::fmt::Debug for GameRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.games.keys()).finish()
    }
}
