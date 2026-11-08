//! A minimal bot: connects, queues for a game, and plays random legal moves.
//!
//! ```text
//! cargo run -p ogap-bot-sdk --example random_bot -- ws://127.0.0.1:7878/ws tictactoe 3
//! ```

use ogap_bot_sdk::{Bot, BotError, ClientConfig, Decision, Matchmaking, run};
use ogap_game_core::{GameRng, Seed};
use ogap_protocol::BotInfo;
use serde_json::Value;

/// Picks a uniformly random legal action.
struct Random(GameRng);

impl Bot for Random {
    fn decide(&mut self, decision: &Decision) -> Result<Value, BotError> {
        self.0
            .choose(decision.legal_actions()?)
            .cloned()
            .ok_or(BotError::NoLegalActions)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .unwrap_or_else(|| "ws://127.0.0.1:7878/ws".into());
    let game = args.next().unwrap_or_else(|| "tictactoe".into());
    let games = args.next().map_or(Ok(1), |n| n.parse())?;

    let config = ClientConfig {
        games,
        ..ClientConfig::new(
            url,
            BotInfo::new("example-random", Some("1.0"))?,
            Matchmaking::queue(game),
        )
    };
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let mut bot = Random(GameRng::from_seed(Seed::new(seed as u64)));
    for summary in run(&config, &mut bot).await? {
        eprintln!(
            "game {} as {}: {:?}",
            summary.game_id, summary.seat, summary.result
        );
    }
    Ok(())
}
