//! A reusable test kit that checks a [`Game`] against the engine contract.
//!
//! Every engine should call [`check`] from its tests with a few hundred seeds.
//! It plays random games and verifies, at every step:
//!
//! 1. the same seed yields the same initial state;
//! 2. the state survives a serde round trip unchanged;
//! 3. every seat in `ToAct` has at least one legal action, and every other
//!    seat has none;
//! 4. every legal action is accepted (when `check_all_legal` is set);
//! 5. an action by a seat that is not to act is rejected **without mutating
//!    the state**, including after the game has finished;
//! 6. games terminate within `max_steps`;
//! 7. outcomes have one valid rank per seat;
//! 8. replaying the recorded actions from the seed reproduces every
//!    intermediate state hash.

use std::fmt;

use crate::game::{Game, Setup};
use crate::hash::StateHash;
use crate::outcome::{Outcome, Status};
use crate::rng::{GameRng, Seed};
use crate::seat::{Seat, SeatCount};

/// Options for [`check`].
#[derive(Debug, Clone)]
pub struct Options {
    /// Seeds `0..games` are tested.
    pub games: u64,
    /// Upper bound on actions per game before reporting non-termination.
    pub max_steps: usize,
    /// Apply every legal action (on a clone) at every step. Quadratic; turn
    /// off for games with very large action lists.
    pub check_all_legal: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            games: 200,
            max_steps: 10_000,
            check_all_legal: true,
        }
    }
}

/// Summary of a successful run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Report {
    /// Games played.
    pub games: u64,
    /// Total actions applied.
    pub steps: u64,
}

/// A contract violation, with enough context to reproduce it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Seed of the failing game.
    pub seed: Seed,
    /// Number of actions applied before the failure.
    pub step: usize,
    /// What went wrong.
    pub message: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "seed {} step {}: {}", self.seed, self.step, self.message)
    }
}

impl std::error::Error for Violation {}

struct Ctx {
    seed: Seed,
    step: usize,
}

impl Ctx {
    fn fail(&self, message: impl Into<String>) -> Violation {
        Violation {
            seed: self.seed,
            step: self.step,
            message: message.into(),
        }
    }
}

fn hash<G: Game>(ctx: &Ctx, state: &G::State) -> Result<StateHash, Violation> {
    StateHash::of(state).map_err(|e| ctx.fail(format!("state does not serialize: {e}")))
}

/// Runs the conformance checks.
///
/// # Errors
///
/// Returns the first [`Violation`] found.
pub fn check<G: Game>(
    game: &G,
    config: &G::Config,
    seats: SeatCount,
    options: &Options,
) -> Result<Report, Violation> {
    let mut report = Report::default();
    for i in 0..options.games {
        let steps = check_one(game, config, seats, Seed::new(i), options)?;
        report.games += 1;
        report.steps += steps as u64;
    }
    Ok(report)
}

fn check_one<G: Game>(
    game: &G,
    config: &G::Config,
    seats: SeatCount,
    seed: Seed,
    options: &Options,
) -> Result<usize, Violation> {
    let mut ctx = Ctx { seed, step: 0 };
    let setup = Setup::new(game, config.clone(), seats, seed)
        .map_err(|e| ctx.fail(format!("setup rejected: {e}")))?;

    let mut state = game.new_game(&setup);
    if hash::<G>(&ctx, &state)? != hash::<G>(&ctx, &game.new_game(&setup))? {
        return Err(ctx.fail("same seed produced different initial states"));
    }

    let mut driver = GameRng::from_seed(seed.derive(0xC0F0_4D4E));
    let mut history: Vec<(Seat, G::Action, StateHash)> = Vec::new();

    loop {
        check_round_trip::<G>(&ctx, &state)?;
        match game.status(&state) {
            Status::Finished(outcome) => {
                check_finished(game, &ctx, &mut state, seats, outcome, &history)?;
                break;
            }
            Status::ToAct(active) => {
                if ctx.step >= options.max_steps {
                    return Err(ctx.fail("game did not terminate"));
                }
                let mut sample = None;
                for seat in seats.seats() {
                    let legal = game.legal_actions(&state, seat);
                    if active.contains(seat) {
                        if legal.is_empty() {
                            return Err(
                                ctx.fail(format!("{seat} must act but has no legal actions"))
                            );
                        }
                        if options.check_all_legal {
                            check_all_accepted(game, &ctx, &state, seat, &legal)?;
                        }
                        sample.get_or_insert_with(|| legal[0].clone());
                    } else if !legal.is_empty() {
                        return Err(ctx.fail(format!("{seat} is not to act but has legal actions")));
                    }
                }
                if let Some(action) = &sample {
                    for seat in seats.seats().filter(|s| !active.contains(*s)) {
                        check_rejected_unchanged(game, &ctx, &mut state, seat, action)?;
                    }
                }

                let seat_ix = driver.index(active.len()).unwrap_or(0);
                let Some(seat) = active.iter().nth(seat_ix) else {
                    return Err(ctx.fail("active seat set is empty"));
                };
                let legal = game.legal_actions(&state, seat);
                let Some(action) = driver.choose(&legal).cloned() else {
                    return Err(ctx.fail(format!("{seat} has no legal actions")));
                };
                game.apply(&mut state, seat, &action).map_err(|e| {
                    ctx.fail(format!("legal action {action:?} by {seat} rejected: {e}"))
                })?;
                ctx.step += 1;
                history.push((seat, action, hash::<G>(&ctx, &state)?));
            }
        }
    }

    check_replay(game, &ctx, &setup, &history)?;
    Ok(ctx.step)
}

fn check_round_trip<G: Game>(ctx: &Ctx, state: &G::State) -> Result<(), Violation> {
    let json = serde_json::to_string(state)
        .map_err(|e| ctx.fail(format!("state does not serialize: {e}")))?;
    let back: G::State = serde_json::from_str(&json)
        .map_err(|e| ctx.fail(format!("state does not deserialize: {e}")))?;
    if hash::<G>(ctx, &back)? != hash::<G>(ctx, state)? {
        return Err(ctx.fail("state changed across a serde round trip"));
    }
    Ok(())
}

fn check_all_accepted<G: Game>(
    game: &G,
    ctx: &Ctx,
    state: &G::State,
    seat: Seat,
    legal: &[G::Action],
) -> Result<(), Violation> {
    for action in legal {
        let mut probe = state.clone();
        game.apply(&mut probe, seat, action)
            .map_err(|e| ctx.fail(format!("legal action {action:?} by {seat} rejected: {e}")))?;
    }
    Ok(())
}

fn check_rejected_unchanged<G: Game>(
    game: &G,
    ctx: &Ctx,
    state: &mut G::State,
    seat: Seat,
    action: &G::Action,
) -> Result<(), Violation> {
    let before = hash::<G>(ctx, state)?;
    if game.apply(state, seat, action).is_ok() {
        return Err(ctx.fail(format!("{seat} acted out of turn with {action:?}")));
    }
    if hash::<G>(ctx, state)? != before {
        return Err(ctx.fail(format!("rejected action by {seat} mutated the state")));
    }
    Ok(())
}

fn check_finished<G: Game>(
    game: &G,
    ctx: &Ctx,
    state: &mut G::State,
    seats: SeatCount,
    outcome: Outcome<G::Score>,
    history: &[(Seat, G::Action, StateHash)],
) -> Result<(), Violation> {
    Outcome::from_ranks(outcome.ranks().to_vec(), seats, ())
        .map_err(|e| ctx.fail(format!("invalid outcome: {e}")))?;
    for seat in seats.seats() {
        if !game.legal_actions(state, seat).is_empty() {
            return Err(ctx.fail(format!("{seat} has legal actions after the game ended")));
        }
        if let Some((_, action, _)) = history.last() {
            check_rejected_unchanged(game, ctx, state, seat, action)?;
        }
    }
    Ok(())
}

fn check_replay<G: Game>(
    game: &G,
    ctx: &Ctx,
    setup: &Setup<G::Config>,
    history: &[(Seat, G::Action, StateHash)],
) -> Result<(), Violation> {
    let mut state = game.new_game(setup);
    for (i, (seat, action, expected)) in history.iter().enumerate() {
        let at = Ctx {
            seed: ctx.seed,
            step: i,
        };
        game.apply(&mut state, *seat, action)
            .map_err(|e| at.fail(format!("replay rejected {action:?}: {e}")))?;
        if hash::<G>(&at, &state)? != *expected {
            return Err(at.fail("replay diverged from the recorded state hash"));
        }
    }
    Ok(())
}
