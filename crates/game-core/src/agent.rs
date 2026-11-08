//! In-process agents for headless simulation.
//!
//! Agents see exactly what a networked cogling would see: their seat's
//! [`Game::PlayerView`] and the legal actions. They never get the
//! authoritative state, so information boundaries hold in simulation too.

use crate::game::Game;
use crate::rng::GameRng;

/// A decision-maker that plays a seat in-process.
pub trait Agent<G: Game>: Send {
    /// Short identifier, e.g. `"random"`.
    fn name(&self) -> &str;

    /// Chooses an action. `legal` is never empty when called by the runner.
    /// Returning `None` (or an illegal action) forfeits the game.
    ///
    /// `rng` is a per-game, per-seat deterministic stream, so simulations are
    /// reproducible from their seed.
    fn act(
        &mut self,
        view: &G::PlayerView,
        legal: &[G::Action],
        rng: &mut GameRng,
    ) -> Option<G::Action>;
}

/// Plays a uniformly random legal action. Works for every game.
#[derive(Debug, Clone, Copy, Default)]
pub struct RandomAgent;

impl<G: Game> Agent<G> for RandomAgent {
    fn name(&self) -> &str {
        "random"
    }

    fn act(
        &mut self,
        _view: &G::PlayerView,
        legal: &[G::Action],
        rng: &mut GameRng,
    ) -> Option<G::Action> {
        rng.choose(legal).cloned()
    }
}
