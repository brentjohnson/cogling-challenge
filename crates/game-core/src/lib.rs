//! Game-engine contract for the Open Game Agent Platform.
//!
//! This crate is deliberately free of IO, async and networking. It defines:
//!
//! - [`Game`]: the statically typed contract each engine implements;
//! - [`DynGame`] / [`DynState`] / [`GameRegistry`]: a JSON-facing, type-erased
//!   view used by servers and tools that pick games at runtime;
//! - [`Seat`], [`Status`], [`Outcome`], [`GameResult`]: shared vocabulary;
//! - [`GameRng`] / [`Seed`]: frozen, serializable randomness for reproducible
//!   games;
//! - [`Agent`]: in-process players for headless simulation;
//! - [`conformance`]: a test kit every engine runs.

mod agent;
pub mod conformance;
mod erased;
mod game;
mod hash;
mod outcome;
mod rng;
mod seat;

pub use agent::{Agent, RandomAgent};
pub use erased::{DynError, DynGame, DynState, GameRegistry, RegistryError};
pub use game::{ConfigError, Game, GameInfo, IllegalAction, Setup};
pub use hash::{ParseStateHashError, StateHash};
pub use outcome::{
    ForfeitPolicy, ForfeitReason, GameResult, Outcome, OutcomeError, Rank, Status, ranks_from_keys,
};
pub use rng::{GameRng, Seed};
pub use seat::{ActiveSeats, MAX_SEATS, Seat, SeatCount, SeatError, SeatRange};

#[cfg(test)]
mod tests {
    //! Exercises the contract machinery with a toy game.

    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;

    /// "Race to N": seats take turns adding 1 or 2 to a shared counter; whoever
    /// reaches the target wins. The starting seat is drawn from the RNG.
    struct Race;

    const RACE_INFO: GameInfo = GameInfo {
        id: "race",
        name: "Race",
        engine_version: "0.0.0",
        ruleset: "standard",
        seats: SeatRange { min: 2, max: 4 },
        description: "toy game",
    };

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RaceConfig {
        target: u32,
    }

    impl Default for RaceConfig {
        fn default() -> Self {
            Self { target: 10 }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct RaceState {
        count: u32,
        target: u32,
        to_move: Seat,
        seats: SeatCount,
        winner: Option<Seat>,
        moves: Vec<u32>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Add(u32);

    impl Game for Race {
        type Config = RaceConfig;
        type State = RaceState;
        type Action = Add;
        type PlayerView = RaceState;
        type PublicView = u32;
        type Score = u32;

        fn info(&self) -> &GameInfo {
            &RACE_INFO
        }

        fn validate_config(&self, c: &RaceConfig, _: SeatCount) -> Result<(), ConfigError> {
            if c.target == 0 {
                return Err(ConfigError::Invalid("target must be positive".into()));
            }
            Ok(())
        }

        fn new_game(&self, setup: &Setup<RaceConfig>) -> RaceState {
            let mut rng = GameRng::from_seed(setup.seed());
            let first = rng.below(u64::from(setup.seats().get())).unwrap_or(0);
            RaceState {
                count: 0,
                target: setup.config().target,
                to_move: Seat::new(u8::try_from(first).unwrap_or(0)).unwrap_or(Seat::FIRST),
                seats: setup.seats(),
                winner: None,
                moves: Vec::new(),
            }
        }

        fn status(&self, s: &RaceState) -> Status<u32> {
            match s.winner {
                Some(w) => Status::Finished(Outcome::winner(w, s.seats, s.count)),
                None => Status::ToAct(ActiveSeats::one(s.to_move)),
            }
        }

        fn legal_actions(&self, s: &RaceState, seat: Seat) -> Vec<Add> {
            if s.winner.is_some() || seat != s.to_move {
                return Vec::new();
            }
            vec![Add(1), Add(2)]
        }

        fn apply(&self, s: &mut RaceState, seat: Seat, a: &Add) -> Result<(), IllegalAction> {
            if s.winner.is_some() {
                return Err(IllegalAction::GameFinished);
            }
            if seat != s.to_move {
                return Err(IllegalAction::NotYourTurn(seat));
            }
            if !(1..=2).contains(&a.0) {
                return Err(IllegalAction::rules("add 1 or 2"));
            }
            s.count += a.0;
            s.moves.push(a.0);
            if s.count >= s.target {
                s.winner = Some(seat);
            } else {
                s.to_move = seat.next(s.seats);
            }
            Ok(())
        }

        fn player_view(&self, s: &RaceState, _: Seat) -> RaceState {
            s.clone()
        }

        fn public_view(&self, s: &RaceState) -> u32 {
            s.count
        }
    }

    #[test]
    fn toy_game_passes_conformance() {
        for n in 2..=4 {
            let report = conformance::check(
                &Race,
                &RaceConfig::default(),
                SeatCount::new(n).unwrap(),
                &conformance::Options::default(),
            )
            .unwrap();
            assert_eq!(report.games, 200);
            assert!(report.steps >= 200 * 5);
        }
    }

    #[test]
    fn conformance_catches_invalid_setup() {
        let err = conformance::check(
            &Race,
            &RaceConfig { target: 0 },
            SeatCount::TWO,
            &conformance::Options::default(),
        )
        .unwrap_err();
        assert!(err.message.contains("target must be positive"), "{err}");
    }

    /// A broken engine that mutates state before rejecting an action.
    struct Leaky;

    impl Game for Leaky {
        type Config = RaceConfig;
        type State = RaceState;
        type Action = Add;
        type PlayerView = RaceState;
        type PublicView = u32;
        type Score = u32;

        fn info(&self) -> &GameInfo {
            &RACE_INFO
        }
        fn validate_config(&self, c: &RaceConfig, s: SeatCount) -> Result<(), ConfigError> {
            Race.validate_config(c, s)
        }
        fn new_game(&self, setup: &Setup<RaceConfig>) -> RaceState {
            Race.new_game(setup)
        }
        fn status(&self, s: &RaceState) -> Status<u32> {
            Race.status(s)
        }
        fn legal_actions(&self, s: &RaceState, seat: Seat) -> Vec<Add> {
            Race.legal_actions(s, seat)
        }
        fn apply(&self, s: &mut RaceState, seat: Seat, a: &Add) -> Result<(), IllegalAction> {
            s.moves.push(999);
            Race.apply(s, seat, a)
        }
        fn player_view(&self, s: &RaceState, _: Seat) -> RaceState {
            s.clone()
        }
        fn public_view(&self, s: &RaceState) -> u32 {
            s.count
        }
    }

    #[test]
    fn conformance_catches_mutation_on_rejection() {
        let err = conformance::check(
            &Leaky,
            &RaceConfig::default(),
            SeatCount::TWO,
            &conformance::Options::default(),
        )
        .unwrap_err();
        assert!(err.message.contains("mutated"), "{err}");
    }

    #[test]
    fn registry_and_erased_api() {
        let mut reg = GameRegistry::new();
        reg.register(Race).unwrap();
        assert_eq!(reg.register(Race), Err(RegistryError::Duplicate("race")));
        let game = reg.get("race").unwrap();
        assert_eq!(game.default_config().unwrap(), json!({"target": 10}));
        assert!(reg.get("chess").is_none());

        // Config parsing: malformed, invalid, seat range.
        let two = SeatCount::TWO;
        assert!(matches!(
            game.create(Some(&json!({"target": "x"})), two, Seed::new(1)),
            Err(DynError::Config(ConfigError::Malformed(_)))
        ));
        assert!(matches!(
            game.create(Some(&json!({"target": 0})), two, Seed::new(1)),
            Err(DynError::Config(ConfigError::Invalid(_)))
        ));
        assert!(matches!(
            game.create(None, SeatCount::new(5).unwrap(), Seed::new(1)),
            Err(DynError::Config(ConfigError::SeatCount { got: 5, .. }))
        ));
        assert_eq!(
            game.normalize_config(Some(&json!({"target": 3})), two)
                .unwrap(),
            json!({"target": 3})
        );

        let mut state = game
            .create(Some(&json!({"target": 3})), two, Seed::new(1))
            .unwrap();
        let Status::ToAct(active) = state.status().unwrap() else {
            panic!("game should be running");
        };
        let seat = active.iter().next().unwrap();
        let other = seat.next(two);
        assert_eq!(state.legal_actions(seat).unwrap(), [json!(1), json!(2)]);
        assert!(state.legal_actions(other).unwrap().is_empty());

        let before = state.state_hash().unwrap();
        let err = state.apply(seat, &json!("one")).unwrap_err();
        assert_eq!(err.code(), "malformed_action");
        let err = state.apply(other, &json!(1)).unwrap_err();
        assert_eq!(err.code(), "not_your_turn");
        let err = state.apply(Seat::new(9).unwrap(), &json!(1)).unwrap_err();
        assert_eq!(err.code(), "no_such_seat");
        assert_eq!(state.state_hash().unwrap(), before);

        state.apply(seat, &json!(2)).unwrap();
        state.apply(other, &json!(1)).unwrap();
        let Status::Finished(outcome) = state.status().unwrap() else {
            panic!("game should be over");
        };
        assert_eq!(outcome.winners().collect::<Vec<_>>(), [other]);
        assert_eq!(outcome.scores(), &json!(3));
        assert_eq!(state.public_view().unwrap(), json!(3));
        assert_eq!(state.info().id, "race");
        assert_eq!(state.config(), &json!({"target": 3}));
        assert!(state.provisional_ranks().is_none());
        assert_eq!(format!("{reg:?}"), r#"["race"]"#);
    }

    #[test]
    fn random_agent_picks_legal() {
        let mut rng = GameRng::from_seed(Seed::new(3));
        let legal = [Add(1), Add(2)];
        let view = RaceState {
            count: 0,
            target: 3,
            to_move: Seat::FIRST,
            seats: SeatCount::TWO,
            winner: None,
            moves: vec![],
        };
        let mut agent = RandomAgent;
        for _ in 0..20 {
            let a = <RandomAgent as Agent<Race>>::act(&mut agent, &view, &legal, &mut rng).unwrap();
            assert!(legal.contains(&a));
        }
        assert!(<RandomAgent as Agent<Race>>::act(&mut agent, &view, &[], &mut rng).is_none());
        assert_eq!(<RandomAgent as Agent<Race>>::name(&agent), "random");
    }
}
