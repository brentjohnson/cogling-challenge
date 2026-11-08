//! Reference agents for Connect Four.

use ogap_game_core::{Agent, GameRng, RandomAgent, Seat, SeatCount};

use crate::{Action, Board, ConnectFour, PlayerView};

/// Names accepted by [`by_name`].
pub const NAMES: [&str; 2] = ["random", "rules"];

/// Looks up a reference agent by name (`"random"` or `"rules"`).
#[must_use]
pub fn by_name(name: &str) -> Option<Box<dyn Agent<ConnectFour>>> {
    match name {
        "random" => Some(Box::new(RandomAgent)),
        "rules" => Some(Box::new(Rules)),
        _ => None,
    }
}

/// Rule-based play: win if possible; otherwise block an immediate threat;
/// otherwise avoid moves that let the opponent win directly on top; prefer
/// columns near the centre (ties broken randomly).
#[derive(Debug, Clone, Copy, Default)]
pub struct Rules;

/// The column [`Rules`] would play on `board` as `me`, or `None` if the
/// board is full. Exposed so networked example bots can reuse it.
pub fn rules_move(board: &Board, me: Seat, rng: &mut GameRng) -> Option<u8> {
    let them = me.next(SeatCount::TWO);
    let open: Vec<u8> = board.open_columns().collect();
    if let Some(&c) = open.iter().find(|&&c| board.wins_with(c, me)) {
        return Some(c);
    }
    if let Some(&c) = open.iter().find(|&&c| board.wins_with(c, them)) {
        return Some(c);
    }
    let gives_away = |c: u8| {
        let mut next = board.clone();
        next.drop_disc(c, me).is_some() && next.wins_with(c, them)
    };
    let safe: Vec<u8> = open.iter().copied().filter(|&c| !gives_away(c)).collect();
    let pool = if safe.is_empty() { open } else { safe };
    // Distance from the centre, doubled to stay in integers.
    let centre = i32::from(board.columns()) - 1;
    let distance = |c: u8| (2 * i32::from(c) - centre).abs();
    let best = pool.iter().map(|&c| distance(c)).min()?;
    let candidates: Vec<u8> = pool.into_iter().filter(|&c| distance(c) == best).collect();
    rng.choose(&candidates).copied()
}

impl Agent<ConnectFour> for Rules {
    fn name(&self) -> &str {
        "rules"
    }

    fn act(&mut self, view: &PlayerView, legal: &[Action], rng: &mut GameRng) -> Option<Action> {
        let column = rules_move(&view.board, view.seat, rng)?;
        let action = Action { column };
        legal.contains(&action).then_some(action)
    }
}

#[cfg(test)]
mod tests {
    use ogap_game_core::Seed;

    use super::*;

    const S0: Seat = Seat::FIRST;
    const S1: Seat = Seat::SECOND;

    fn board(moves: &[(u8, Seat)]) -> Board {
        let mut b = Board::new(7, 6);
        for &(c, s) in moves {
            b.drop_disc(c, s).unwrap();
        }
        b
    }

    #[test]
    fn wins_then_blocks() {
        let mut rng = GameRng::from_seed(Seed::new(0));
        // Both sides have three on the bottom row; the mover takes its own win.
        let b = board(&[(0, S0), (0, S1), (1, S0), (1, S1), (2, S0), (2, S1)]);
        assert_eq!(rules_move(&b, S0, &mut rng), Some(3));
        assert_eq!(rules_move(&b, S1, &mut rng), Some(3));
        // Only S0 threatens: S1 blocks.
        let b = board(&[(0, S0), (6, S1), (1, S0), (6, S1), (2, S0)]);
        assert_eq!(rules_move(&b, S1, &mut rng), Some(3));
    }

    #[test]
    fn avoids_giving_away_a_win_on_top() {
        let mut rng = GameRng::from_seed(Seed::new(0));
        // S1 has three on row 1 in columns 0..3 (over S0 discs); playing
        // column 3 as S0 would let S1 complete row 1 at (3,1).
        let b = board(&[
            (0, S0),
            (0, S1),
            (1, S0),
            (1, S1),
            (2, S0),
            (2, S1),
            (6, S0),
            (6, S1),
            (5, S0),
            (5, S1),
        ]);
        // S0 to move. Column 3 wins for S0 on row 0, so it is taken first.
        assert_eq!(rules_move(&b, S0, &mut rng), Some(3));
        // Row 0: S0 S1 S0 . . . S0; row 1: S1 S1 S1 . . . S0. Nobody can win
        // immediately, but S0 playing column 3 lets S1 complete row 1 at (3,1).
        let b = board(&[
            (0, S0),
            (1, S1),
            (2, S0),
            (0, S1),
            (1, S1),
            (2, S1),
            (6, S0),
            (6, S0),
        ]);
        for _ in 0..20 {
            let mv = rules_move(&b, S0, &mut rng).unwrap();
            assert_ne!(mv, 3, "column 3 hands S1 row 1");
        }
    }

    #[test]
    fn prefers_centre() {
        let mut rng = GameRng::from_seed(Seed::new(0));
        assert_eq!(rules_move(&Board::new(7, 6), S0, &mut rng), Some(3));
        let even = rules_move(&Board::new(6, 6), S0, &mut rng).unwrap();
        assert!([2, 3].contains(&even));
    }

    #[test]
    fn registry_of_agents() {
        for name in NAMES {
            assert_eq!(by_name(name).unwrap().name(), name);
        }
        assert!(by_name("mcts").is_none());
    }
}
