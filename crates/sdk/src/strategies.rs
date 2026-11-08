//! Ready-made example strategies.
//!
//! - [`RandomCogling`] works for every game (needs `state_and_legal` mode).
//! - [`TicTacToeRules`] and [`ConnectFourRules`] parse the typed observation
//!   and reuse the engines' reference heuristics, showing how a cogling can work
//!   from the observation alone.

use cogling_game_core::{GameRng, Seed};
use serde_json::Value;

use crate::{Cogling, CoglingError, Decision};

/// Strategy names accepted by [`for_game`].
pub const NAMES: [&str; 2] = ["random", "rules"];

/// Builds a strategy by game and name, seeding its randomness with `seed`.
#[must_use]
pub fn for_game(game: &str, strategy: &str, seed: Seed) -> Option<Box<dyn Cogling>> {
    let rng = GameRng::from_seed(seed);
    match (game, strategy) {
        (_, "random") => Some(Box::new(RandomCogling { rng })),
        ("tictactoe", "rules") => Some(Box::new(TicTacToeRules { rng })),
        ("connect4", "rules") => Some(Box::new(ConnectFourRules { rng })),
        _ => None,
    }
}

/// Plays a uniformly random legal action.
#[derive(Debug, Clone)]
pub struct RandomCogling {
    rng: GameRng,
}

impl RandomCogling {
    /// A random cogling with the given seed.
    #[must_use]
    pub const fn new(seed: Seed) -> Self {
        Self {
            rng: GameRng::from_seed(seed),
        }
    }
}

impl Cogling for RandomCogling {
    fn decide(&mut self, decision: &Decision) -> Result<Value, CoglingError> {
        self.rng
            .choose(decision.legal_actions()?)
            .cloned()
            .ok_or(CoglingError::NoLegalActions)
    }
}

fn parse<T: serde::de::DeserializeOwned>(observation: &Value) -> Result<T, CoglingError> {
    T::deserialize(observation).map_err(|e| CoglingError::Observation(e.to_string()))
}

/// Tic-Tac-Toe: win, block, centre, corners. Needs only the observation.
#[derive(Debug, Clone)]
pub struct TicTacToeRules {
    rng: GameRng,
}

impl Cogling for TicTacToeRules {
    fn decide(&mut self, decision: &Decision) -> Result<Value, CoglingError> {
        let view: cogling_tictactoe::PlayerView = parse(&decision.observation)?;
        let cell = cogling_tictactoe::agents::rules_move(&view.board, view.mark, &mut self.rng)
            .ok_or(CoglingError::NoLegalActions)?;
        Ok(serde_json::json!({ "cell": u8::from(cell) }))
    }
}

/// Connect Four: win, block, avoid giving away wins, prefer the centre.
#[derive(Debug, Clone)]
pub struct ConnectFourRules {
    rng: GameRng,
}

impl Cogling for ConnectFourRules {
    fn decide(&mut self, decision: &Decision) -> Result<Value, CoglingError> {
        let view: cogling_connect4::PlayerView = parse(&decision.observation)?;
        let column = cogling_connect4::agents::rules_move(&view.board, view.seat, &mut self.rng)
            .ok_or(CoglingError::NoLegalActions)?;
        Ok(serde_json::json!({ "column": column }))
    }
}

#[cfg(test)]
mod tests {
    use cogling_game_core::{Game, Seat, SeatCount, Setup};
    use cogling_protocol::DecisionId;
    use serde_json::json;

    use super::*;

    fn decision(observation: Value, legal: Option<Vec<Value>>) -> Decision {
        Decision {
            seat: Seat::FIRST,
            decision_id: DecisionId::FIRST,
            remaining_ms: 1_000,
            observation,
            legal_actions: legal,
        }
    }

    #[test]
    fn lookup() {
        assert!(for_game("tictactoe", "rules", Seed::new(0)).is_some());
        assert!(for_game("connect4", "rules", Seed::new(0)).is_some());
        assert!(for_game("anything", "random", Seed::new(0)).is_some());
        assert!(for_game("chess", "rules", Seed::new(0)).is_none());
    }

    #[test]
    fn random_needs_legal_actions() {
        let mut cogling = RandomCogling::new(Seed::new(1));
        assert!(matches!(
            cogling.decide(&decision(json!({}), None)),
            Err(CoglingError::LegalActionsRequired)
        ));
        assert!(matches!(
            cogling.decide(&decision(json!({}), Some(vec![]))),
            Err(CoglingError::NoLegalActions)
        ));
        let legal = vec![json!({"cell": 3}), json!({"cell": 5})];
        let action = cogling
            .decide(&decision(json!({}), Some(legal.clone())))
            .unwrap();
        assert!(legal.contains(&action));
    }

    #[test]
    fn rules_coglings_read_real_observations() {
        let ttt = cogling_tictactoe::TicTacToe;
        let setup = Setup::new(&ttt, Default::default(), SeatCount::TWO, Seed::new(0)).unwrap();
        let state = ttt.new_game(&setup);
        let obs = serde_json::to_value(ttt.player_view(&state, Seat::FIRST)).unwrap();
        let mut cogling = for_game("tictactoe", "rules", Seed::new(0)).unwrap();
        assert_eq!(
            cogling.decide(&decision(obs, None)).unwrap(),
            json!({"cell": 4})
        );

        let c4 = cogling_connect4::ConnectFour;
        let setup = Setup::new(&c4, Default::default(), SeatCount::TWO, Seed::new(0)).unwrap();
        let state = c4.new_game(&setup);
        let obs = serde_json::to_value(c4.player_view(&state, Seat::FIRST)).unwrap();
        let mut cogling = for_game("connect4", "rules", Seed::new(0)).unwrap();
        assert_eq!(
            cogling.decide(&decision(obs, None)).unwrap(),
            json!({"column": 3})
        );

        assert!(matches!(
            cogling.decide(&decision(json!({"nonsense": true}), None)),
            Err(CoglingError::Observation(_))
        ));
    }
}
