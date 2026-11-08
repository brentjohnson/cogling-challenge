//! The statically typed game-engine contract.

use std::fmt::Debug;

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::outcome::{Rank, Status};
use crate::rng::Seed;
use crate::seat::{Seat, SeatCount, SeatRange};

/// Static metadata describing a game implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GameInfo {
    /// Stable machine identifier, e.g. `"connect4"`. Used in protocols and logs.
    pub id: &'static str,
    /// Human-readable name.
    pub name: &'static str,
    /// Engine implementation version (normally the crate version). Replays
    /// require an exact match.
    pub engine_version: &'static str,
    /// Identifier of the base ruleset implemented, e.g. `"standard"`.
    /// Variants are expressed through the configuration.
    pub ruleset: &'static str,
    /// Supported seat counts.
    pub seats: SeatRange,
    /// One-line description.
    pub description: &'static str,
}

/// A configuration was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The game does not support this many seats.
    #[error("{game} supports {min}..={max} seats, got {got}")]
    SeatCount {
        /// Game id.
        game: &'static str,
        /// Requested seat count.
        got: u8,
        /// Minimum supported.
        min: u8,
        /// Maximum supported.
        max: u8,
    },
    /// The configuration is well-formed but has invalid values.
    #[error("invalid configuration: {0}")]
    Invalid(String),
    /// The configuration could not be parsed.
    #[error("malformed configuration: {0}")]
    Malformed(String),
}

/// An action was rejected by the engine. The state is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum IllegalAction {
    /// The seat is not currently required to act.
    #[error("it is not {0}'s turn")]
    NotYourTurn(Seat),
    /// The game is already over.
    #[error("the game is finished")]
    GameFinished,
    /// The action violates the rules; the message explains why.
    #[error("{0}")]
    Rules(String),
}

impl IllegalAction {
    /// Stable machine-readable code, used in the bot protocol.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotYourTurn(_) => "not_your_turn",
            Self::GameFinished => "game_finished",
            Self::Rules(_) => "illegal_action",
        }
    }

    /// Convenience constructor for rule violations.
    pub fn rules(msg: impl Into<String>) -> Self {
        Self::Rules(msg.into())
    }
}

/// A validated game setup: configuration, seat count and seed.
///
/// Can only be built through [`Setup::new`], which checks the seat count and
/// runs the game's own configuration validation, so engines may rely on a
/// `Setup` being valid ("parse, don't validate").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup<C> {
    config: C,
    seats: SeatCount,
    seed: Seed,
}

impl<C> Setup<C> {
    /// Validates a setup for `game`.
    ///
    /// # Errors
    ///
    /// [`ConfigError::SeatCount`] if `seats` is outside the game's range, or
    /// whatever [`Game::validate_config`] reports.
    pub fn new<G>(game: &G, config: C, seats: SeatCount, seed: Seed) -> Result<Self, ConfigError>
    where
        G: Game<Config = C>,
    {
        let info = game.info();
        if !info.seats.contains(seats) {
            return Err(ConfigError::SeatCount {
                game: info.id,
                got: seats.get(),
                min: info.seats.min,
                max: info.seats.max,
            });
        }
        game.validate_config(&config, seats)?;
        Ok(Self {
            config,
            seats,
            seed,
        })
    }

    /// The validated configuration.
    #[must_use]
    pub const fn config(&self) -> &C {
        &self.config
    }

    /// Number of seats.
    #[must_use]
    pub const fn seats(&self) -> SeatCount {
        self.seats
    }

    /// The game seed.
    #[must_use]
    pub const fn seed(&self) -> Seed {
        self.seed
    }
}

/// The contract every game engine implements.
///
/// Engines are pure: no IO, no clocks, no threads, no ambient randomness.
/// All randomness must come from a [`crate::GameRng`] seeded from
/// [`Setup::seed`] and stored in the state, so that the same setup and the
/// same sequence of accepted actions always produce the same state.
///
/// The authoritative [`Game::State`] is never sent to bots; they receive a
/// [`Game::PlayerView`] containing only what their seat may know.
///
/// Use [`crate::conformance`] to check an implementation against the
/// contract.
pub trait Game: Send + Sync + 'static {
    /// Game configuration. `Default` must be the canonical ruleset.
    type Config: Serialize + DeserializeOwned + Default + Clone + Debug + Send + Sync;
    /// Authoritative state, including any RNG state. Must serialize
    /// deterministically (avoid `HashMap`).
    type State: Serialize + DeserializeOwned + Clone + Debug + Send + Sync;
    /// A single decision a player can make.
    type Action: Serialize + DeserializeOwned + Clone + Eq + Debug + Send + Sync;
    /// Everything one seat is entitled to know.
    type PlayerView: Serialize;
    /// Everything a spectator is entitled to know.
    type PublicView: Serialize;
    /// Game-specific score detail attached to the outcome.
    type Score: Serialize + Clone + Debug;

    /// Static metadata.
    fn info(&self) -> &GameInfo;

    /// Game-specific configuration checks; seat range is already checked.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Invalid`] describing the problem.
    fn validate_config(&self, config: &Self::Config, seats: SeatCount) -> Result<(), ConfigError>;

    /// Creates the initial state.
    fn new_game(&self, setup: &Setup<Self::Config>) -> Self::State;

    /// Who must act next, or the outcome if the game is over.
    fn status(&self, state: &Self::State) -> Status<Self::Score>;

    /// All legal actions for `seat`. Empty if `seat` is not required to act.
    /// Must be non-empty for every seat in [`Status::ToAct`].
    fn legal_actions(&self, state: &Self::State, seat: Seat) -> Vec<Self::Action>;

    /// Validates and applies `action` by `seat`.
    ///
    /// # Errors
    ///
    /// Returns [`IllegalAction`] if the action is not legal. **The state must
    /// be left unchanged on error.**
    fn apply(
        &self,
        state: &mut Self::State,
        seat: Seat,
        action: &Self::Action,
    ) -> Result<(), IllegalAction>;

    /// The observation for `seat`.
    fn player_view(&self, state: &Self::State, seat: Seat) -> Self::PlayerView;

    /// The spectator observation.
    fn public_view(&self, state: &Self::State) -> Self::PublicView;

    /// Current standing of an unfinished game (lower is better), used to rank
    /// the remaining seats when someone forfeits under
    /// [`crate::ForfeitPolicy::Provisional`]. `None` if the game has no
    /// meaningful standing.
    fn provisional_ranks(&self, _state: &Self::State) -> Option<Vec<Rank>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn illegal_action_codes() {
        assert_eq!(
            IllegalAction::NotYourTurn(Seat::FIRST).code(),
            "not_your_turn"
        );
        assert_eq!(IllegalAction::GameFinished.code(), "game_finished");
        assert_eq!(IllegalAction::rules("no").code(), "illegal_action");
        assert_eq!(IllegalAction::rules("cell taken").to_string(), "cell taken");
    }
}
