//! Reference agents for Tic-Tac-Toe.

use cogling_game_core::{Agent, GameRng, RandomAgent};

use crate::{Action, Board, Cell, Mark, PlayerView, TicTacToe};

/// Names accepted by [`by_name`].
pub const NAMES: [&str; 2] = ["random", "rules"];

/// Looks up a reference agent by name (`"random"` or `"rules"`).
#[must_use]
pub fn by_name(name: &str) -> Option<Box<dyn Agent<TicTacToe>>> {
    match name {
        "random" => Some(Box::new(RandomAgent)),
        "rules" => Some(Box::new(Rules)),
        _ => None,
    }
}

/// Rule-based play: win if possible, otherwise block, otherwise prefer the
/// centre, then corners, then edges (ties broken randomly).
#[derive(Debug, Clone, Copy, Default)]
pub struct Rules;

/// The move [`Rules`] would make on `board` playing `mark`, or `None` if the
/// board is full. Exposed so networked example coglings can reuse it.
pub fn rules_move(board: &Board, mark: Mark, rng: &mut GameRng) -> Option<Cell> {
    let empty: Vec<Cell> = board.empty_cells().collect();
    if let Some(&win) = empty.iter().find(|c| board.wins_with(**c, mark)) {
        return Some(win);
    }
    if let Some(&block) = empty.iter().find(|c| board.wins_with(**c, mark.opponent())) {
        return Some(block);
    }
    let priority = |c: &Cell| match c.index() {
        4 => 0,
        0 | 2 | 6 | 8 => 1,
        _ => 2,
    };
    let best = empty.iter().map(priority).min()?;
    let candidates: Vec<Cell> = empty.into_iter().filter(|c| priority(c) == best).collect();
    rng.choose(&candidates).copied()
}

impl Agent<TicTacToe> for Rules {
    fn name(&self) -> &str {
        "rules"
    }

    fn act(&mut self, view: &PlayerView, legal: &[Action], rng: &mut GameRng) -> Option<Action> {
        let cell = rules_move(&view.board, view.mark, rng)?;
        let action = Action { cell };
        legal.contains(&action).then_some(action)
    }
}

#[cfg(test)]
mod tests {
    use cogling_game_core::Seed;

    use super::*;

    fn board(s: &str) -> Board {
        let mut b = Board::default();
        for (i, ch) in s.chars().filter(|c| !c.is_whitespace()).enumerate() {
            b.0[i] = match ch {
                'X' => Some(Mark::X),
                'O' => Some(Mark::O),
                _ => None,
            };
        }
        b
    }

    #[test]
    fn takes_the_win_over_the_block() {
        let mut rng = GameRng::from_seed(Seed::new(0));
        let b = board("XX. OO. ...");
        assert_eq!(rules_move(&b, Mark::X, &mut rng), Cell::new(2).ok());
        assert_eq!(rules_move(&b, Mark::O, &mut rng), Cell::new(5).ok());
    }

    #[test]
    fn blocks() {
        let mut rng = GameRng::from_seed(Seed::new(0));
        // X threatens the left column; O must take cell 6.
        let b = board("X.. X.. .O.");
        assert_eq!(rules_move(&b, Mark::O, &mut rng), Cell::new(6).ok());
    }

    #[test]
    fn prefers_centre_then_corner() {
        let mut rng = GameRng::from_seed(Seed::new(0));
        assert_eq!(
            rules_move(&Board::default(), Mark::X, &mut rng),
            Cell::new(4).ok()
        );
        let corner = rules_move(&board("... .X. ..."), Mark::O, &mut rng).unwrap();
        assert!([0, 2, 6, 8].contains(&corner.index()));
    }

    #[test]
    fn full_board_has_no_move() {
        let mut rng = GameRng::from_seed(Seed::new(0));
        assert_eq!(rules_move(&board("XOX XOO OXX"), Mark::X, &mut rng), None);
    }

    #[test]
    fn registry_of_agents() {
        for name in NAMES {
            assert_eq!(by_name(name).unwrap().name(), name);
        }
        assert!(by_name("minimax").is_none());
    }
}
