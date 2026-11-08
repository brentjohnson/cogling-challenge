//! Connect Four for the Open Game Agent Platform.
//!
//! Standard rules: two seats take turns dropping a disc into a column; the
//! disc falls to the lowest empty row. Four in a row horizontally,
//! vertically or diagonally wins; a full board is a draw.
//!
//! The canonical board is 7 columns × 6 rows. Other sizes from 4 to 12 in
//! either dimension are available as variants. Which seat moves first is
//! configurable (`first_seat`, default seat 0).
//!
//! An action is `{"column": c}` with `0 <= c < columns`. In views, the board
//! is `cells`, a flat row-major array starting at the **bottom** row:
//! `cells[row * columns + column]`, each entry a seat number or `null`.

pub mod agents;

use ogap_game_core::{
    ActiveSeats, ConfigError, Game, GameInfo, IllegalAction, Outcome, Seat, SeatCount, SeatRange,
    Setup, Status,
};
use serde::{Deserialize, Serialize};

const INFO: GameInfo = GameInfo {
    id: "connect4",
    name: "Connect Four",
    engine_version: env!("CARGO_PKG_VERSION"),
    ruleset: "standard",
    seats: SeatRange::exactly(2),
    description: "Drop discs to connect four in a row",
};

/// Discs in a row needed to win.
pub const CONNECT: usize = 4;
/// Smallest allowed board dimension.
pub const MIN_DIMENSION: u8 = 4;
/// Largest allowed board dimension.
pub const MAX_DIMENSION: u8 = 12;

/// The Connect Four engine.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConnectFour;

/// Configuration. The default is the canonical 7×6 game with seat 0 first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Seat that moves first.
    pub first_seat: Seat,
    /// Board width.
    pub columns: u8,
    /// Board height.
    pub rows: u8,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            first_seat: Seat::FIRST,
            columns: 7,
            rows: 6,
        }
    }
}

/// A board position. Row 0 is the bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pos {
    /// Column, left to right.
    pub column: u8,
    /// Row, bottom to top.
    pub row: u8,
}

/// The board: dimensions plus a flat, bottom-up, row-major cell array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    columns: u8,
    rows: u8,
    cells: Vec<Option<Seat>>,
}

impl Board {
    /// An empty board.
    #[must_use]
    pub fn new(columns: u8, rows: u8) -> Self {
        Self {
            columns,
            rows,
            cells: vec![None; usize::from(columns) * usize::from(rows)],
        }
    }

    /// Board width.
    #[must_use]
    pub const fn columns(&self) -> u8 {
        self.columns
    }

    /// Board height.
    #[must_use]
    pub const fn rows(&self) -> u8 {
        self.rows
    }

    fn index(&self, column: i32, row: i32) -> Option<usize> {
        let in_bounds = (0..i32::from(self.columns)).contains(&column)
            && (0..i32::from(self.rows)).contains(&row);
        // Both coordinates are non-negative and bounded by u8 dimensions here.
        in_bounds.then(|| (row * i32::from(self.columns) + column).unsigned_abs() as usize)
    }

    /// The disc at `(column, row)`, or `None` if empty or off the board.
    #[must_use]
    pub fn get(&self, column: i32, row: i32) -> Option<Seat> {
        self.index(column, row)
            .and_then(|i| self.cells.get(i).copied().flatten())
    }

    /// The row a disc dropped into `column` would land on, or `None` if the
    /// column is full or does not exist.
    #[must_use]
    pub fn landing_row(&self, column: u8) -> Option<u8> {
        if column >= self.columns {
            return None;
        }
        (0..self.rows).find(|&r| self.get(i32::from(column), i32::from(r)).is_none())
    }

    /// Columns that are not full, left to right.
    pub fn open_columns(&self) -> impl Iterator<Item = u8> + '_ {
        (0..self.columns).filter(|&c| self.landing_row(c).is_some())
    }

    /// Whether every cell is occupied.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.cells.iter().all(Option::is_some)
    }

    /// Drops a disc for `seat`, returning where it landed.
    pub fn drop_disc(&mut self, column: u8, seat: Seat) -> Option<Pos> {
        let row = self.landing_row(column)?;
        let i = self.index(i32::from(column), i32::from(row))?;
        let cell = self.cells.get_mut(i)?;
        *cell = Some(seat);
        Some(Pos { column, row })
    }

    /// A line of [`CONNECT`] discs through `pos` owned by its occupant.
    #[must_use]
    pub fn line_through(&self, pos: Pos) -> Option<[Pos; CONNECT]> {
        let (c, r) = (i32::from(pos.column), i32::from(pos.row));
        let seat = self.get(c, r)?;
        for (dc, dr) in [(1, 0), (0, 1), (1, 1), (1, -1)] {
            // Walk back to the start of the run, then forward collecting it.
            let mut start = 0;
            while self.get(c - (start + 1) * dc, r - (start + 1) * dr) == Some(seat) {
                start += 1;
            }
            let run: Vec<Pos> = (0..)
                .map(|k| (c + (k - start) * dc, r + (k - start) * dr))
                .take_while(|&(x, y)| self.get(x, y) == Some(seat))
                .filter_map(|(x, y)| {
                    Some(Pos {
                        column: u8::try_from(x).ok()?,
                        row: u8::try_from(y).ok()?,
                    })
                })
                .collect();
            if let Some(line) = run.first_chunk::<CONNECT>() {
                return Some(*line);
            }
        }
        None
    }

    /// Whether dropping a disc for `seat` into `column` would win.
    #[must_use]
    pub fn wins_with(&self, column: u8, seat: Seat) -> bool {
        let mut next = self.clone();
        next.drop_disc(column, seat)
            .is_some_and(|pos| next.line_through(pos).is_some())
    }
}

/// How a finished game ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Finish {
    /// A seat connected four.
    Win {
        /// The winner.
        seat: Seat,
        /// The connecting line.
        line: [Pos; CONNECT],
    },
    /// The board filled up.
    Draw,
}

/// Authoritative state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    board: Board,
    to_move: Seat,
    finish: Option<Finish>,
}

/// Drop a disc into a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    /// Target column, `0..columns`.
    pub column: u8,
}

/// Game-specific score detail: how the game ended.
pub type Score = Finish;

/// What a seat sees. Connect Four has no hidden information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerView {
    /// The board.
    pub board: Board,
    /// The observing seat.
    pub seat: Seat,
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

impl ConnectFour {
    fn to_move(state: &State) -> Option<Seat> {
        state.finish.is_none().then_some(state.to_move)
    }
}

impl Game for ConnectFour {
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
        let range = MIN_DIMENSION..=MAX_DIMENSION;
        if !range.contains(&config.columns) || !range.contains(&config.rows) {
            return Err(ConfigError::Invalid(format!(
                "board must be between {MIN_DIMENSION} and {MAX_DIMENSION} in each dimension, got {}x{}",
                config.columns, config.rows
            )));
        }
        if !seats.contains(config.first_seat) {
            return Err(ConfigError::Invalid(format!(
                "first_seat {} does not exist",
                config.first_seat.get()
            )));
        }
        Ok(())
    }

    fn new_game(&self, setup: &Setup<Config>) -> State {
        let config = setup.config();
        State {
            board: Board::new(config.columns, config.rows),
            to_move: config.first_seat,
            finish: None,
        }
    }

    fn status(&self, state: &State) -> Status<Score> {
        match &state.finish {
            Some(finish @ Finish::Win { seat, .. }) => {
                Status::Finished(Outcome::winner(*seat, SeatCount::TWO, finish.clone()))
            }
            Some(Finish::Draw) => Status::Finished(Outcome::draw(SeatCount::TWO, Finish::Draw)),
            None => Status::ToAct(ActiveSeats::one(state.to_move)),
        }
    }

    fn legal_actions(&self, state: &State, seat: Seat) -> Vec<Action> {
        if Self::to_move(state) != Some(seat) {
            return Vec::new();
        }
        state
            .board
            .open_columns()
            .map(|column| Action { column })
            .collect()
    }

    fn apply(&self, state: &mut State, seat: Seat, action: &Action) -> Result<(), IllegalAction> {
        if state.finish.is_some() {
            return Err(IllegalAction::GameFinished);
        }
        if seat != state.to_move {
            return Err(IllegalAction::NotYourTurn(seat));
        }
        if action.column >= state.board.columns {
            return Err(IllegalAction::rules(format!(
                "column {} is out of range (0..={})",
                action.column,
                state.board.columns - 1
            )));
        }
        let Some(pos) = state.board.drop_disc(action.column, seat) else {
            return Err(IllegalAction::rules(format!(
                "column {} is full",
                action.column
            )));
        };
        state.finish = match state.board.line_through(pos) {
            Some(line) => Some(Finish::Win { seat, line }),
            None if state.board.is_full() => Some(Finish::Draw),
            None => None,
        };
        state.to_move = seat.next(SeatCount::TWO);
        Ok(())
    }

    fn player_view(&self, state: &State, seat: Seat) -> PlayerView {
        PlayerView {
            board: state.board.clone(),
            seat,
            to_move: Self::to_move(state),
        }
    }

    fn public_view(&self, state: &State) -> PublicView {
        PublicView {
            board: state.board.clone(),
            to_move: Self::to_move(state),
        }
    }
}

#[cfg(test)]
mod tests {
    use ogap_game_core::{Seed, conformance};
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    const S0: Seat = Seat::FIRST;
    const S1: Seat = Seat::SECOND;

    fn new_state(config: Config) -> State {
        let setup = Setup::new(&ConnectFour, config, SeatCount::TWO, Seed::new(0)).unwrap();
        ConnectFour.new_game(&setup)
    }

    fn play(columns: &[u8]) -> State {
        play_on(Config::default(), columns)
    }

    fn play_on(config: Config, columns: &[u8]) -> State {
        let mut state = new_state(config);
        for &column in columns {
            let seat = state.to_move;
            ConnectFour
                .apply(&mut state, seat, &Action { column })
                .unwrap();
        }
        state
    }

    fn winner(state: &State) -> Option<Seat> {
        match &state.finish {
            Some(Finish::Win { seat, .. }) => Some(*seat),
            _ => None,
        }
    }

    #[test]
    fn conformance_variants() {
        let small = Config {
            columns: 4,
            rows: 4,
            ..Config::default()
        };
        let tall = Config {
            first_seat: S1,
            columns: 5,
            rows: 8,
        };
        for config in [Config::default(), small, tall] {
            conformance::check(
                &ConnectFour,
                &config,
                SeatCount::TWO,
                &conformance::Options {
                    games: 100,
                    ..conformance::Options::default()
                },
            )
            .unwrap();
        }
    }

    #[test]
    fn horizontal_win() {
        let state = play(&[0, 0, 1, 1, 2, 2, 3]);
        assert_eq!(winner(&state), Some(S0));
        let Some(Finish::Win { line, .. }) = &state.finish else {
            panic!()
        };
        assert_eq!(
            line.map(|p| (p.column, p.row)),
            [(0, 0), (1, 0), (2, 0), (3, 0)]
        );
    }

    #[test]
    fn vertical_win_for_second_seat() {
        let state = play(&[0, 1, 0, 1, 2, 1, 3, 1]);
        assert_eq!(winner(&state), Some(S1));
    }

    #[test]
    fn diagonal_wins_both_directions() {
        // Rising diagonal (0,0)-(3,3) for seat 0.
        let rising = play(&[0, 1, 1, 2, 2, 3, 2, 3, 3, 6, 3]);
        assert_eq!(winner(&rising), Some(S0));
        // Falling diagonal (3,0)-(0,3) for seat 0, completed in the middle.
        let falling = play(&[3, 2, 2, 1, 1, 0, 1, 0, 0, 6, 6, 5, 0]);
        let Some(Finish::Win { seat, line }) = &falling.finish else {
            panic!("expected a win, got {:?}", falling.finish);
        };
        assert_eq!(*seat, S0);
        let mut cells: Vec<(u8, u8)> = line.iter().map(|p| (p.column, p.row)).collect();
        cells.sort_unstable();
        assert_eq!(cells, [(0, 3), (1, 2), (2, 1), (3, 0)]);
    }

    #[test]
    fn win_detected_when_last_disc_fills_a_gap() {
        // Seat 0 holds columns 0,1,3 on the bottom row and wins by filling 2.
        let state = play(&[0, 0, 1, 1, 3, 3, 2]);
        assert_eq!(winner(&state), Some(S0));
    }

    #[test]
    fn draw_on_small_board() {
        let config = Config {
            columns: 4,
            rows: 4,
            ..Config::default()
        };
        let state = play_on(config, &[0, 2, 1, 3, 2, 0, 3, 1, 0, 2, 1, 3, 2, 0, 3, 1]);
        assert_eq!(state.finish, Some(Finish::Draw));
        let Status::Finished(o) = ConnectFour.status(&state) else {
            panic!()
        };
        assert_eq!(o.winners().count(), 2);
    }

    #[test]
    fn illegal_moves() {
        let game = ConnectFour;
        let mut state = play(&[0, 0, 0, 0, 0, 0]);
        let before = state.clone();
        let err = game
            .apply(&mut state, S0, &Action { column: 0 })
            .unwrap_err();
        assert!(err.to_string().contains("full"), "{err}");
        let err = game
            .apply(&mut state, S0, &Action { column: 7 })
            .unwrap_err();
        assert!(err.to_string().contains("out of range"), "{err}");
        let err = game
            .apply(&mut state, S1, &Action { column: 1 })
            .unwrap_err();
        assert_eq!(err, IllegalAction::NotYourTurn(S1));
        assert_eq!(state, before);
        assert!(
            !game
                .legal_actions(&state, S0)
                .contains(&Action { column: 0 })
        );
        assert_eq!(game.legal_actions(&state, S0).len(), 6);

        let mut done = play(&[0, 1, 0, 1, 0, 1, 0]);
        assert_eq!(
            game.apply(&mut done, S1, &Action { column: 1 }),
            Err(IllegalAction::GameFinished)
        );
    }

    #[test]
    fn config_validation() {
        for (columns, rows) in [(3, 6), (7, 13), (13, 4)] {
            let err = Setup::new(
                &ConnectFour,
                Config {
                    columns,
                    rows,
                    ..Config::default()
                },
                SeatCount::TWO,
                Seed::new(0),
            )
            .unwrap_err();
            assert!(matches!(err, ConfigError::Invalid(_)));
        }
        assert_eq!(
            serde_json::from_value::<Config>(json!({"rows": 5})).unwrap(),
            Config {
                rows: 5,
                ..Config::default()
            }
        );
        assert!(serde_json::from_value::<Config>(json!({"connect": 5})).is_err());
    }

    #[test]
    fn first_seat_moves_first() {
        let state = play_on(
            Config {
                first_seat: S1,
                ..Config::default()
            },
            &[3],
        );
        assert_eq!(state.board.get(3, 0), Some(S1));
        assert_eq!(state.to_move, S0);
    }

    #[test]
    fn view_json_shape() {
        let state = play_on(
            Config {
                columns: 4,
                rows: 4,
                ..Config::default()
            },
            &[1, 1],
        );
        let view = serde_json::to_value(ConnectFour.player_view(&state, S0)).unwrap();
        assert_eq!(view["board"]["columns"], json!(4));
        assert_eq!(view["board"]["cells"][1], json!(0));
        assert_eq!(view["board"]["cells"][5], json!(1));
        assert_eq!(view["board"]["cells"][0], json!(null));
        assert_eq!(view["to_move"], json!(0));
        let back: PlayerView = serde_json::from_value(view).unwrap();
        assert_eq!(back.board, state.board);
    }

    proptest! {
        /// Arbitrary column sequences never break gravity or the move count.
        #[test]
        fn gravity_holds(columns in proptest::collection::vec(0_u8..9, 0..60)) {
            let game = ConnectFour;
            let mut state = new_state(Config::default());
            let mut accepted = 0_usize;
            for column in columns {
                let seat = state.to_move;
                let before = state.clone();
                match game.apply(&mut state, seat, &Action { column }) {
                    Ok(()) => accepted += 1,
                    Err(_) => prop_assert_eq!(&state, &before),
                }
            }
            let discs = state.board.cells.iter().filter(|c| c.is_some()).count();
            prop_assert_eq!(discs, accepted);
            for c in 0..7 {
                let mut seen_empty = false;
                for r in 0..6 {
                    let filled = state.board.get(c, r).is_some();
                    prop_assert!(!(seen_empty && filled), "floating disc at ({c},{r})");
                    seen_empty |= !filled;
                }
            }
        }
    }
}
