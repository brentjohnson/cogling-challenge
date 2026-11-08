//! `ogap`: run local game servers, simulations, replays and example bots.

use std::io::Write;
use std::process::ExitCode;

use clap::Parser;
use ogap_cli::{Cli, Command, GameCommand};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let default_level = match cli.command {
        Command::Serve(_) | Command::Bot(_) => "info",
        _ => "warn",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level)),
        )
        .with_writer(std::io::stderr)
        .init();

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        // Output piped into `head` and similar: stop quietly.
        Err(e) if is_broken_pipe(&e) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn is_broken_pipe(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
    })
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    match cli.command {
        Command::Serve(args) => ogap_cli::serve(args).await,
        Command::Games => ogap_cli::games(&mut out),
        Command::Sim(args) => ogap_cli::sim(&args, &mut out).map(drop),
        Command::Replay(args) => ogap_cli::replay(&args, &mut out),
        Command::Game(GameCommand::Create(args)) => {
            let id = ogap_cli::create_game(args).await?;
            writeln!(out, "{id}")?;
            Ok(())
        }
        Command::Bot(args) => {
            for s in ogap_cli::bot(args).await? {
                let outcome = if s.won() { "won" } else { "did not win" };
                writeln!(
                    out,
                    "{} as {}: {} ({})",
                    s.game_id,
                    s.seat,
                    outcome,
                    serde_json::to_string(&s.result)?
                )?;
            }
            Ok(())
        }
    }
}
