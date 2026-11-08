//! Tic-Tac-Toe for Cogling Challenge.
//!
//! Standard rules on a 3×3 board. Seat 0 plays `X`, seat 1 plays `O`; which
//! seat moves first is configurable (`first_seat`, default seat 0).
//!
//! Cells are numbered row-major:
//!
//! ```text
//! 0 | 1 | 2
//! 3 | 4 | 5
//! 6 | 7 | 8
//! ```
//!
//! An action is `{"cell": n}`.

pub mod agents;

use std::fmt;

use cogling_game_core::{
    ActiveSeats, ConfigError, Game, GameInfo, IllegalAction, Outcome, Seat, SeatCount, SeatRange,
    Setup, Status,
};
use serde::{Deserialize, Serialize};

const INFO: GameInfo = GameInfo {
    id: "tictactoe",
    name: "Tic-Tac-Toe",
    engine_version: env!("CARGO_PKG_VERSION"),
    ruleset: "standard",
    seats: SeatRange::exactly(2),
    description: "Three in a row on a 3x3 grid",
};

/// The eight winning lines.
pub const LINES: [[Cell; 3]; 8] = {
    const fn c(i: u8) -> Cell {
        Cell(i)
    }
    [
        [c(0), c(1), c(2)],
        [c(3), c(4), c(5)],
        [c(6), c(7), c(8)],
        [c(0), c(3), c(6)],
        [c(1), c(4), c(7)],
        [c(2), c(5), c(8)],
        [c(0), c(4), c(8)],
        [c(2), c(4), c(6)],
    ]
};

/// The Tic-Tac-Toe engine.
#[derive(Debug, Clone, Copy, Default)]
pub struct TicTacToe;

/// Configuration. The default is the canonical game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Seat that moves first.
    pub first_seat: Seat,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            first_seat: Seat::FIRST,
        }
    }
}

/// A player's mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mark {
    /// Played by seat 0.
    X,
    /// Played by seat 1.
    O,
}

impl Mark {
    /// The mark played by `seat`.
    #[must_use]
    pub const fn of(seat: Seat) -> Self {
        if seat.get() == 0 { Self::X } else { Self::O }
    }

    /// The seat playing this mark.
    #[must_use]
    pub const fn seat(self) -> Seat {
        match self {
            Self::X => Seat::FIRST,
            Self::O => Seat::SECOND,
        }
    }

    /// The opposing mark.
    #[must_use]
    pub const fn opponent(self) -> Self {
        match self {
            Self::X => Self::O,
            Self::O => Self::X,
        }
    }
}

/// A board cell index, guaranteed to be `0..9`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Cell(u8);

/// Error for a cell index outside `0..9`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellOutOfRange(pub u8);

impl fmt::Display for CellOutOfRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cell {} is out of range (0..=8)", self.0)
    }
}

impl std::error::Error for CellOutOfRange {}

impl Cell {
    /// All nine cells in order.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..9).map(Self)
    }

    /// Creates a cell.
    ///
    /// # Errors
    ///
    /// [`CellOutOfRange`] if `index > 8`.
    pub const fn new(index: u8) -> Result<Self, CellOutOfRange> {
        if index < 9 {
            Ok(Self(index))
        } else {
            Err(CellOutOfRange(index))
        }
    }

    /// The index as `usize`, for board indexing.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl TryFrom<u8> for Cell {
    type Error = CellOutOfRange;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Cell> for u8 {
    fn from(cell: Cell) -> Self {
        cell.0
    }
}

/// The 3×3 board, row-major. `None` is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Board(pub [Option<Mark>; 9]);

impl Board {
    /// The mark at `cell`.
    #[must_use]
    pub const fn get(&self, cell: Cell) -> Option<Mark> {
        self.0[cell.index()]
    }

    /// Empty cells in order.
    pub fn empty_cells(&self) -> impl Iterator<Item = Cell> + '_ {
        Cell::all().filter(|c| self.get(*c).is_none())
    }

    /// The completed line, if any.
    #[must_use]
    pub fn winning_line(&self) -> Option<(Mark, [Cell; 3])> {
        LINES.iter().find_map(|line| {
            let [a, b, c] = line.map(|cell| self.get(cell));
            match (a, b, c) {
                (Some(x), Some(y), Some(z)) if x == y && y == z => Some((x, *line)),
                _ => None,
            }
        })
    }

    /// Whether placing `mark` at the empty `cell` completes a line.
    #[must_use]
    pub fn wins_with(&self, cell: Cell, mark: Mark) -> bool {
        if self.get(cell).is_some() {
            return false;
        }
        let mut next = *self;
        next.0[cell.index()] = Some(mark);
        next.winning_line().is_some_and(|(m, _)| m == mark)
    }

    fn is_full(&self) -> bool {
        self.0.iter().all(Option::is_some)
    }
}

/// Authoritative state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    board: Board,
    to_move: Seat,
}

/// Place a mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    /// Target cell.
    pub cell: Cell,
}

/// Game-specific score detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Score {
    /// The completed line, or `None` for a draw.
    pub winning_line: Option<[Cell; 3]>,
}

/// What a seat sees. Tic-Tac-Toe has no hidden information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerView {
    /// The board.
    pub board: Board,
    /// The observing seat.
    pub seat: Seat,
    /// The observing seat's mark.
    pub mark: Mark,
    /// Seat to move, or `None` if the game is over.
    pub to_move: Option<Seat>,
}

/// What a spectator sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicView {
    /// The board.
    pub board: Board,
    /// Seat to move, or `None` if the game is over.
    pub to_move: Option<Seat>,
}

impl TicTacToe {
    fn is_over(state: &State) -> bool {
        state.board.winning_line().is_some() || state.board.is_full()
    }

    fn to_move(state: &State) -> Option<Seat> {
        (!Self::is_over(state)).then_some(state.to_move)
    }
}

impl Game for TicTacToe {
    type Config = Config;
    type State = State;
    type Action = Action;
    type PlayerView = PlayerView;
    type PublicView = PublicView;
    type Score = Score;

    fn info(&self) -> &GameInfo {
        &INFO
    }

    fn validate_config(&self, config: &Config, seats: SeatCount) -> Result<(), ConfigError> {
        if seats.contains(config.first_seat) {
            Ok(())
        } else {
            Err(ConfigError::Invalid(format!(
                "first_seat {} does not exist",
                config.first_seat.get()
            )))
        }
    }

    fn new_game(&self, setup: &Setup<Config>) -> State {
        State {
            board: Board::default(),
            to_move: setup.config().first_seat,
        }
    }

    fn status(&self, state: &State) -> Status<Score> {
        if let Some((mark, line)) = state.board.winning_line() {
            Status::Finished(Outcome::winner(
                mark.seat(),
                SeatCount::TWO,
                Score {
                    winning_line: Some(line),
                },
            ))
        } else if state.board.is_full() {
            Status::Finished(Outcome::draw(SeatCount::TWO, Score { winning_line: None }))
        } else {
            Status::ToAct(ActiveSeats::one(state.to_move))
        }
    }

    fn legal_actions(&self, state: &State, seat: Seat) -> Vec<Action> {
        if Self::to_move(state) != Some(seat) {
            return Vec::new();
        }
        state
            .board
            .empty_cells()
            .map(|cell| Action { cell })
            .collect()
    }

    fn apply(&self, state: &mut State, seat: Seat, action: &Action) -> Result<(), IllegalAction> {
        if Self::is_over(state) {
            return Err(IllegalAction::GameFinished);
        }
        if seat != state.to_move {
            return Err(IllegalAction::NotYourTurn(seat));
        }
        if let Some(mark) = state.board.get(action.cell) {
            return Err(IllegalAction::rules(format!(
                "cell {} is already taken by {mark:?}",
                action.cell.0
            )));
        }
        state.board.0[action.cell.index()] = Some(Mark::of(seat));
        state.to_move = seat.next(SeatCount::TWO);
        Ok(())
    }

    fn player_view(&self, state: &State, seat: Seat) -> PlayerView {
        PlayerView {
            board: state.board,
            seat,
            mark: Mark::of(seat),
            to_move: Self::to_move(state),
        }
    }

    fn public_view(&self, state: &State) -> PublicView {
        PublicView {
            board: state.board,
            to_move: Self::to_move(state),
        }
    }
}

#[cfg(test)]
mod tests {
    use cogling_game_core::{Seed, conformance};
    use serde_json::json;

    use super::*;

    fn setup(first: u8) -> Setup<Config> {
        Setup::new(
            &TicTacToe,
            Config {
                first_seat: Seat::new(first).unwrap(),
            },
            SeatCount::TWO,
            Seed::new(0),
        )
        .unwrap()
    }

    fn play(first: u8, cells: &[u8]) -> State {
        let game = TicTacToe;
        let mut state = game.new_game(&setup(first));
        for &c in cells {
            let seat = state.to_move;
            game.apply(&mut state, seat, &Action { cell: Cell(c) })
                .unwrap();
        }
        state
    }

    fn ranks(state: &State) -> Option<Vec<u8>> {
        match TicTacToe.status(state) {
            Status::Finished(o) => Some(o.ranks().iter().map(|r| r.get()).collect()),
            Status::ToAct(_) => None,
        }
    }

    #[test]
    fn conformance_both_first_seats() {
        for first in 0..2 {
            let config = Config {
                first_seat: Seat::new(first).unwrap(),
            };
            conformance::check(
                &TicTacToe,
                &config,
                SeatCount::TWO,
                &conformance::Options::default(),
            )
            .unwrap();
        }
    }

    #[test]
    fn every_line_wins() {
        for line in LINES {
            // X plays the line; O plays two cells off the line.
            let others: Vec<u8> = Cell::all()
                .filter(|c| !line.contains(c))
                .map(u8::from)
                .collect();
            let moves = [line[0].0, others[0], line[1].0, others[1], line[2].0];
            // Pick O's cells so they never complete a line themselves.
            let state = play(0, &moves);
            let Status::Finished(outcome) = TicTacToe.status(&state) else {
                panic!("line {line:?} should finish the game");
            };
            assert_eq!(outcome.winners().collect::<Vec<_>>(), [Seat::FIRST]);
            assert_eq!(outcome.scores().winning_line, Some(line));
        }
    }

    #[test]
    fn o_can_win_and_first_seat_is_respected() {
        // Seat 1 (O) moves first and wins on the anti-diagonal.
        let state = play(1, &[2, 0, 4, 1, 6]);
        assert_eq!(ranks(&state), Some(vec![2, 1]));
        assert_eq!(state.board.get(Cell(2)), Some(Mark::O));
    }

    #[test]
    fn draw() {
        // X O X / X O O / O X X
        let state = play(0, &[0, 1, 2, 4, 3, 5, 7, 6, 8]);
        assert_eq!(ranks(&state), Some(vec![1, 1]));
        let Status::Finished(o) = TicTacToe.status(&state) else {
            panic!()
        };
        assert_eq!(o.scores().winning_line, None);
    }

    #[test]
    fn illegal_moves_are_rejected() {
        let game = TicTacToe;
        let mut state = play(0, &[4]);
        let err = game
            .apply(&mut state, Seat::SECOND, &Action { cell: Cell(4) })
            .unwrap_err();
        assert_eq!(err.code(), "illegal_action");
        assert!(err.to_string().contains("already taken"));
        let err = game
            .apply(&mut state, Seat::FIRST, &Action { cell: Cell(0) })
            .unwrap_err();
        assert_eq!(err, IllegalAction::NotYourTurn(Seat::FIRST));

        let mut done = play(0, &[0, 3, 1, 4, 2]);
        let err = game
            .apply(&mut done, Seat::SECOND, &Action { cell: Cell(8) })
            .unwrap_err();
        assert_eq!(err, IllegalAction::GameFinished);
        assert!(game.legal_actions(&done, Seat::SECOND).is_empty());
    }

    #[test]
    fn action_json_is_parsed_strictly() {
        assert_eq!(
            serde_json::from_value::<Action>(json!({"cell": 8})).unwrap(),
            Action { cell: Cell(8) }
        );
        assert!(serde_json::from_value::<Action>(json!({"cell": 9})).is_err());
        assert!(serde_json::from_value::<Action>(json!({"cell": -1})).is_err());
        assert!(serde_json::from_value::<Action>(json!({"cell": 1, "x": 0})).is_err());
        assert!(serde_json::from_value::<Action>(json!(4)).is_err());
    }

    #[test]
    fn config_validation() {
        let err = Setup::new(
            &TicTacToe,
            Config {
                first_seat: Seat::new(2).unwrap(),
            },
            SeatCount::TWO,
            Seed::new(0),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
        let err = Setup::new(
            &TicTacToe,
            Config::default(),
            SeatCount::new(3).unwrap(),
            Seed::new(0),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::SeatCount { got: 3, .. }));
        assert!(serde_json::from_value::<Config>(json!({"bogus": 1})).is_err());
        assert_eq!(
            serde_json::from_value::<Config>(json!({})).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn views_json_shape() {
        let game = TicTacToe;
        let state = play(0, &[4, 0]);
        let view = serde_json::to_value(game.player_view(&state, Seat::SECOND)).unwrap();
        assert_eq!(
            view,
            json!({
                "board": ["O", null, null, null, "X", null, null, null, null],
                "seat": 1,
                "mark": "O",
                "to_move": 0
            })
        );
        let public = serde_json::to_value(game.public_view(&state)).unwrap();
        assert_eq!(public["to_move"], json!(0));
        let back: PlayerView = serde_json::from_value(view).unwrap();
        assert_eq!(back.mark, Mark::O);
    }
}
