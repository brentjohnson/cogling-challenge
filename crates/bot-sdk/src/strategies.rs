//! Ready-made example strategies.
//!
//! - [`RandomBot`] works for every game (needs `state_and_legal` mode).
//! - [`TicTacToeRules`] and [`ConnectFourRules`] parse the typed observation
//!   and reuse the engines' reference heuristics, showing how a bot can work
//!   from the observation alone.

use ogap_game_core::{GameRng, Seed};
use serde_json::Value;

use crate::{Bot, BotError, Decision};

/// Strategy names accepted by [`for_game`].
pub const NAMES: [&str; 2] = ["random", "rules"];

/// Builds a strategy by game and name, seeding its randomness with `seed`.
#[must_use]
pub fn for_game(game: &str, strategy: &str, seed: Seed) -> Option<Box<dyn Bot>> {
    let rng = GameRng::from_seed(seed);
    match (game, strategy) {
        (_, "random") => Some(Box::new(RandomBot { rng })),
        ("tictactoe", "rules") => Some(Box::new(TicTacToeRules { rng })),
        ("connect4", "rules") => Some(Box::new(ConnectFourRules { rng })),
        _ => None,
    }
}

/// Plays a uniformly random legal action.
#[derive(Debug, Clone)]
pub struct RandomBot {
    rng: GameRng,
}

impl RandomBot {
    /// A random bot with the given seed.
    #[must_use]
    pub const fn new(seed: Seed) -> Self {
        Self {
            rng: GameRng::from_seed(seed),
        }
    }
}

impl Bot for RandomBot {
    fn decide(&mut self, decision: &Decision) -> Result<Value, BotError> {
        self.rng
            .choose(decision.legal_actions()?)
            .cloned()
            .ok_or(BotError::NoLegalActions)
    }
}

fn parse<T: serde::de::DeserializeOwned>(observation: &Value) -> Result<T, BotError> {
    T::deserialize(observation).map_err(|e| BotError::Observation(e.to_string()))
}

/// Tic-Tac-Toe: win, block, centre, corners. Needs only the observation.
#[derive(Debug, Clone)]
pub struct TicTacToeRules {
    rng: GameRng,
}

impl Bot for TicTacToeRules {
    fn decide(&mut self, decision: &Decision) -> Result<Value, BotError> {
        let view: ogap_tictactoe::PlayerView = parse(&decision.observation)?;
        let cell = ogap_tictactoe::agents::rules_move(&view.board, view.mark, &mut self.rng)
            .ok_or(BotError::NoLegalActions)?;
        Ok(serde_json::json!({ "cell": u8::from(cell) }))
    }
}

/// Connect Four: win, block, avoid giving away wins, prefer the centre.
#[derive(Debug, Clone)]
pub struct ConnectFourRules {
    rng: GameRng,
}

impl Bot for ConnectFourRules {
    fn decide(&mut self, decision: &Decision) -> Result<Value, BotError> {
        let view: ogap_connect4::PlayerView = parse(&decision.observation)?;
        let column = ogap_connect4::agents::rules_move(&view.board, view.seat, &mut self.rng)
            .ok_or(BotError::NoLegalActions)?;
        Ok(serde_json::json!({ "column": column }))
    }
}

#[cfg(test)]
mod tests {
    use ogap_game_core::{Game, Seat, SeatCount, Setup};
    use ogap_protocol::DecisionId;
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
        let mut bot = RandomBot::new(Seed::new(1));
        assert!(matches!(
            bot.decide(&decision(json!({}), None)),
            Err(BotError::LegalActionsRequired)
        ));
        assert!(matches!(
            bot.decide(&decision(json!({}), Some(vec![]))),
            Err(BotError::NoLegalActions)
        ));
        let legal = vec![json!({"cell": 3}), json!({"cell": 5})];
        let action = bot
            .decide(&decision(json!({}), Some(legal.clone())))
            .unwrap();
        assert!(legal.contains(&action));
    }

    #[test]
    fn rules_bots_read_real_observations() {
        let ttt = ogap_tictactoe::TicTacToe;
        let setup = Setup::new(&ttt, Default::default(), SeatCount::TWO, Seed::new(0)).unwrap();
        let state = ttt.new_game(&setup);
        let obs = serde_json::to_value(ttt.player_view(&state, Seat::FIRST)).unwrap();
        let mut bot = for_game("tictactoe", "rules", Seed::new(0)).unwrap();
        assert_eq!(
            bot.decide(&decision(obs, None)).unwrap(),
            json!({"cell": 4})
        );

        let c4 = ogap_connect4::ConnectFour;
        let setup = Setup::new(&c4, Default::default(), SeatCount::TWO, Seed::new(0)).unwrap();
        let state = c4.new_game(&setup);
        let obs = serde_json::to_value(c4.player_view(&state, Seat::FIRST)).unwrap();
        let mut bot = for_game("connect4", "rules", Seed::new(0)).unwrap();
        assert_eq!(
            bot.decide(&decision(obs, None)).unwrap(),
            json!({"column": 3})
        );

        assert!(matches!(
            bot.decide(&decision(json!({"nonsense": true}), None)),
            Err(BotError::Observation(_))
        ));
    }
}
