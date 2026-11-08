//! Implementation of the `ogap` command-line tool.
//!
//! `main.rs` only parses arguments and reports errors; every command lives
//! here so it can be tested.

use std::fs::File;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use ogap_bot_sdk::{ClientConfig, GameSummary, Matchmaking};
use ogap_connect4::ConnectFour;
use ogap_eventlog::{FileLogStore, GameLog, LogStore, NullLogStore, Replayer};
use ogap_game_core::{
    Agent, ForfeitPolicy, Game, GameRegistry, RandomAgent, Seat, SeatCount, Seed,
};
use ogap_protocol::{BotInfo, ClientMessage, GameId, ObservationMode};
use ogap_server::{Server, ServerConfig, ThinkTime};
use ogap_sim::{BatchConfig, BatchReport, Entrant, Seating};
use ogap_tictactoe::TicTacToe;
use serde_json::{Value, json};

/// Every game shipped with the platform.
///
/// # Errors
///
/// Fails only if two built-in games share an id (a programming error).
pub fn builtin_registry() -> Result<GameRegistry> {
    let mut registry = GameRegistry::new();
    registry.register(TicTacToe)?;
    registry.register(ConnectFour)?;
    Ok(registry)
}

/// The Open Game Agent Platform tool.
#[derive(Debug, Parser)]
#[command(name = "ogap", version, about)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run a local game server.
    Serve(ServeArgs),
    /// List available games.
    Games,
    /// Run a batch of headless simulations with built-in agents.
    Sim(SimArgs),
    /// Verify a game log and print its states.
    Replay(ReplayArgs),
    /// Manage lobby games on a server.
    #[command(subcommand)]
    Game(GameCommand),
    /// Connect a built-in example bot to a server.
    Bot(BotArgs),
}

/// `ogap serve`.
#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:7878")]
    pub bind: String,
    /// Default think time per decision, in milliseconds.
    #[arg(long, default_value_t = 1_000)]
    pub think_time_ms: u64,
    /// Default observation mode.
    #[arg(long, value_enum, default_value_t = ObservationArg::StateAndLegal)]
    pub observation_mode: ObservationArg,
    /// Reconnect grace period, in milliseconds.
    #[arg(long, default_value_t = 10_000)]
    pub reconnect_grace_ms: u64,
    /// How remaining seats are ranked when one forfeits.
    #[arg(long, value_enum, default_value_t = ForfeitArg::Tied)]
    pub forfeit_policy: ForfeitArg,
    /// Directory for game logs.
    #[arg(long, default_value = "logs")]
    pub log_dir: PathBuf,
    /// Do not write game logs.
    #[arg(long)]
    pub no_logs: bool,
}

/// Observation mode on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ObservationArg {
    /// Observation only.
    State,
    /// Observation plus legal actions.
    StateAndLegal,
}

impl From<ObservationArg> for ObservationMode {
    fn from(arg: ObservationArg) -> Self {
        match arg {
            ObservationArg::State => Self::State,
            ObservationArg::StateAndLegal => Self::StateAndLegal,
        }
    }
}

/// Forfeit policy on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ForfeitArg {
    /// Remaining seats share first place.
    Tied,
    /// Remaining seats are ranked by the engine's provisional standing.
    Provisional,
}

impl From<ForfeitArg> for ForfeitPolicy {
    fn from(arg: ForfeitArg) -> Self {
        match arg {
            ForfeitArg::Tied => Self::TiedOthers,
            ForfeitArg::Provisional => Self::Provisional,
        }
    }
}

/// `ogap sim`.
#[derive(Debug, Args)]
pub struct SimArgs {
    /// Game id.
    #[arg(long)]
    pub game: String,
    /// Comma-separated agents, one per seat (e.g. `random,rules`).
    #[arg(long, value_delimiter = ',', required = true)]
    pub agents: Vec<String>,
    /// Number of games.
    #[arg(long, default_value_t = 1_000)]
    pub games: u64,
    /// Batch seed; the same seed reproduces the same results.
    #[arg(long, default_value_t = 0)]
    pub seed: u64,
    /// Worker threads (default: all cores).
    #[arg(long)]
    pub threads: Option<usize>,
    /// Seat assignment policy.
    #[arg(long, value_enum, default_value_t = SeatingArg::Rotate)]
    pub seating: SeatingArg,
    /// Game configuration as JSON (default: canonical rules).
    #[arg(long)]
    pub config: Option<String>,
    /// Write one log per game into this directory.
    #[arg(long)]
    pub log_dir: Option<PathBuf>,
    /// Print the report as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Seating on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SeatingArg {
    /// Rotate seats every game (perfectly balanced).
    Rotate,
    /// Random permutation per game.
    Shuffle,
}

impl From<SeatingArg> for Seating {
    fn from(arg: SeatingArg) -> Self {
        match arg {
            SeatingArg::Rotate => Self::Rotate,
            SeatingArg::Shuffle => Self::Shuffle,
        }
    }
}

/// `ogap replay`.
#[derive(Debug, Args)]
pub struct ReplayArgs {
    /// Log file.
    pub file: PathBuf,
    /// Which view to print after each event: `public`, `full`, or `seat:N`.
    #[arg(long, default_value = "public")]
    pub view: String,
    /// Only verify the log; print a one-line summary.
    #[arg(long)]
    pub verify: bool,
}

/// `ogap game ...`.
#[derive(Debug, Subcommand)]
pub enum GameCommand {
    /// Create a lobby game and print its id.
    Create(CreateArgs),
}

/// `ogap game create`.
#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Server WebSocket URL.
    #[arg(long, default_value = "ws://127.0.0.1:7878/ws")]
    pub url: String,
    /// Game id.
    #[arg(long)]
    pub game: String,
    /// Seat count (default: game minimum).
    #[arg(long)]
    pub seats: Option<u8>,
    /// Game configuration as JSON.
    #[arg(long)]
    pub config: Option<String>,
    /// Think time per decision, in milliseconds.
    #[arg(long)]
    pub think_time_ms: Option<u64>,
    /// Observation mode.
    #[arg(long, value_enum)]
    pub observation_mode: Option<ObservationArg>,
}

/// `ogap bot`.
#[derive(Debug, Args)]
pub struct BotArgs {
    /// Server WebSocket URL.
    #[arg(long, default_value = "ws://127.0.0.1:7878/ws")]
    pub url: String,
    /// Game id.
    #[arg(long)]
    pub game: String,
    /// Strategy: `random` or `rules`.
    #[arg(long, default_value = "random")]
    pub strategy: String,
    /// Bot name (default: `example-<strategy>`).
    #[arg(long)]
    pub name: Option<String>,
    /// Bot version.
    #[arg(long, default_value = env!("CARGO_PKG_VERSION"))]
    pub version: String,
    /// Games to play in a row (quick-match only).
    #[arg(long, default_value_t = 1)]
    pub games: u32,
    /// Join this lobby instead of quick-matching.
    #[arg(long)]
    pub join: Option<GameId>,
    /// Seed for the strategy's randomness (default: random).
    #[arg(long)]
    pub seed: Option<u64>,
}

fn parse_json(text: Option<&str>) -> Result<Option<Value>> {
    text.map(|t| serde_json::from_str(t).context("--config is not valid JSON"))
        .transpose()
}

fn time_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

/// Runs a local server until Ctrl-C.
///
/// # Errors
///
/// Fails on invalid settings or if the address cannot be bound.
pub async fn serve(args: ServeArgs) -> Result<()> {
    let config = ServerConfig {
        think_time: ThinkTime::from_millis(args.think_time_ms)?,
        observation_mode: args.observation_mode.into(),
        reconnect_grace: std::time::Duration::from_millis(args.reconnect_grace_ms),
        forfeit_policy: args.forfeit_policy.into(),
        ..ServerConfig::default()
    };
    let logs: Arc<dyn LogStore> = if args.no_logs {
        Arc::new(NullLogStore)
    } else {
        Arc::new(
            FileLogStore::new(&args.log_dir)
                .with_context(|| format!("creating log directory {}", args.log_dir.display()))?,
        )
    };
    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("binding {}", args.bind))?;
    let addr = listener.local_addr()?;
    tracing::info!(
        %addr,
        logs = %if args.no_logs { "off".into() } else { args.log_dir.display().to_string() },
        "ogap server listening; bots connect to ws://{addr}/ws"
    );
    let server = Server::new(builtin_registry()?, config, logs);
    server
        .serve(listener, async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await
        .context("server failed")
}

/// Writes the list of games.
///
/// # Errors
///
/// Fails if writing to `out` fails.
pub fn games(out: &mut impl Write) -> Result<()> {
    for game in builtin_registry()?.iter() {
        let info = game.info();
        let default_config = game.default_config()?;
        writeln!(
            out,
            "{:<10} {:<14} v{:<7} seats {}-{}  ruleset {:<9} default config {}",
            info.id,
            info.name,
            info.engine_version,
            info.seats.min,
            info.seats.max,
            info.ruleset,
            default_config
        )?;
    }
    Ok(())
}

fn run_batch_for<G: Game>(
    game: &G,
    lookup: fn(&str) -> Option<Box<dyn Agent<G>>>,
    args: &SimArgs,
) -> Result<BatchReport> {
    let config: G::Config = match parse_json(args.config.as_deref())? {
        Some(v) => serde_json::from_value(v).context("invalid --config for this game")?,
        None => G::Config::default(),
    };
    let entrants = args
        .agents
        .iter()
        .map(|name| {
            if lookup(name).is_none() {
                bail!("unknown agent {name:?} for {}", game.info().id);
            }
            let owned = name.clone();
            // The name was validated above, so the fallback is never used.
            Ok(Entrant::new(name.as_str(), move || {
                lookup(&owned).unwrap_or_else(|| Box::new(RandomAgent))
            })?)
        })
        .collect::<Result<Vec<_>>>()?;
    let store = args
        .log_dir
        .as_ref()
        .map(FileLogStore::new)
        .transpose()
        .context("creating log directory")?;
    let batch = BatchConfig {
        games: args.games,
        seed: Seed::new(args.seed),
        seating: args.seating.into(),
        forfeit_policy: ForfeitPolicy::default(),
        threads: args.threads,
    };
    Ok(ogap_sim::run_batch(
        game,
        &config,
        &entrants,
        &batch,
        store.as_ref().map(|s| s as &dyn LogStore),
    )?)
}

/// Runs a simulation batch and writes the report.
///
/// # Errors
///
/// Fails on unknown games or agents, invalid configuration, or IO errors.
pub fn sim(args: &SimArgs, out: &mut impl Write) -> Result<BatchReport> {
    let started = Instant::now();
    let report = match args.game.as_str() {
        "tictactoe" => run_batch_for(&TicTacToe, ogap_tictactoe::agents::by_name, args)?,
        "connect4" => run_batch_for(&ConnectFour, ogap_connect4::agents::by_name, args)?,
        other => bail!("unknown game {other:?}; see `ogap games`"),
    };
    let elapsed = started.elapsed();
    if args.json {
        writeln!(out, "{}", serde_json::to_string_pretty(&report)?)?;
        return Ok(report);
    }
    writeln!(
        out,
        "{} games of {} in {:.2?} ({} actions, {} aborted)",
        report.games, report.game, elapsed, report.total_actions, report.aborted
    )?;
    writeln!(
        out,
        "{:<3} {:<12} {:>7} {:>7} {:>7} {:>7} {:>9} {:>9}  by seat (W-D-L)",
        "#", "agent", "wins", "draws", "losses", "win%", "avg rank", "forfeits"
    )?;
    for (i, e) in report.entrants.iter().enumerate() {
        let games = e.overall.games.max(1);
        // Ratios for display only.
        #[allow(clippy::cast_precision_loss, reason = "display of small counts")]
        let avg_rank = e.rank_sum as f64 / games as f64;
        let by_seat: Vec<String> = e
            .by_seat
            .iter()
            .map(|r| format!("{}-{}-{}", r.wins, r.draws, r.losses))
            .collect();
        writeln!(
            out,
            "{:<3} {:<12} {:>7} {:>7} {:>7} {:>6.1}% {:>9.3} {:>9}  {}",
            i,
            e.label,
            e.overall.wins,
            e.overall.draws,
            e.overall.losses,
            e.overall.win_rate() * 100.0,
            avg_rank,
            e.forfeits,
            by_seat.join("  ")
        )?;
    }
    Ok(report)
}

enum View {
    Public,
    Full,
    Seat(Seat),
}

fn parse_view(text: &str) -> Result<View> {
    match text {
        "public" => Ok(View::Public),
        "full" => Ok(View::Full),
        other => {
            let n = other
                .strip_prefix("seat:")
                .and_then(|n| n.parse::<u8>().ok())
                .ok_or_else(|| anyhow!("--view must be public, full or seat:N"))?;
            Ok(View::Seat(Seat::new(n)?))
        }
    }
}

/// Reads a log file.
///
/// # Errors
///
/// Fails if the file cannot be opened or is not a valid log.
pub fn read_log(path: &Path) -> Result<GameLog> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    GameLog::read(BufReader::new(file)).with_context(|| format!("reading {}", path.display()))
}

/// Replays a log, verifying every state hash, and writes either a summary or
/// one JSON frame per event: `{"seq", "t_ms", "event", "view"}`.
///
/// # Errors
///
/// Fails if the log is invalid or the replay diverges.
pub fn replay(args: &ReplayArgs, out: &mut impl Write) -> Result<()> {
    let log = read_log(&args.file)?;
    let view = parse_view(&args.view)?;
    let registry = builtin_registry()?;
    let mut replayer = Replayer::new(&registry, &log)?;
    if args.verify {
        replayer.run_to_end()?;
        let header = log.header();
        writeln!(
            out,
            "ok: {} {} ({} v{}), {} events, result {}",
            header.game,
            header.game_id,
            header.ruleset,
            header.engine_version,
            log.events().len(),
            log.result()
                .map_or_else(|| "none (unfinished)".into(), |r| json!(r).to_string())
        )?;
        return Ok(());
    }
    if let View::Seat(seat) = view
        && !log.header().seats.contains(seat)
    {
        bail!("{seat} does not exist in this game");
    }
    while let Some(event) = replayer.step()? {
        let state = replayer.state();
        let view = match view {
            View::Public => state.public_view()?,
            View::Full => state.state_json()?,
            View::Seat(seat) => state.player_view(seat)?,
        };
        let frame = json!({
            "seq": event.seq,
            "t_ms": event.t_ms,
            "event": event.body,
            "view": view,
        });
        writeln!(out, "{frame}")?;
    }
    Ok(())
}

/// Creates a lobby and returns its id.
///
/// # Errors
///
/// Fails on invalid arguments or if the server rejects the request.
pub async fn create_game(args: CreateArgs) -> Result<GameId> {
    let bot = BotInfo::new("ogap-cli", Some(env!("CARGO_PKG_VERSION")))?;
    let config = ClientConfig::new(&args.url, bot, Matchmaking::queue(&args.game));
    let request = ClientMessage::CreateGame {
        game: args.game,
        seats: args.seats.map(SeatCount::new).transpose()?,
        config: parse_json(args.config.as_deref())?,
        think_time_ms: args.think_time_ms,
        observation_mode: args.observation_mode.map(Into::into),
    };
    Ok(ogap_bot_sdk::create_game(&config, request).await?)
}

/// Runs a built-in example bot.
///
/// # Errors
///
/// Fails on unknown strategies, invalid names, or connection errors.
pub async fn bot(args: BotArgs) -> Result<Vec<GameSummary>> {
    let seed = Seed::new(args.seed.unwrap_or_else(time_seed));
    let mut strategy = ogap_bot_sdk::strategies::for_game(&args.game, &args.strategy, seed)
        .ok_or_else(|| {
            anyhow!(
                "no {:?} strategy for {:?}; available: {:?}",
                args.strategy,
                args.game,
                ogap_bot_sdk::strategies::NAMES
            )
        })?;
    let name = args
        .name
        .unwrap_or_else(|| format!("example-{}", args.strategy));
    let matchmaking = match args.join {
        Some(id) => Matchmaking::Join(id),
        None => Matchmaking::queue(args.game),
    };
    let config = ClientConfig {
        games: args.games,
        ..ClientConfig::new(
            args.url,
            BotInfo::new(name, Some(&args.version))?,
            matchmaking,
        )
    };
    Ok(ogap_bot_sdk::run(&config, &mut strategy).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sim_args(game: &str, agents: &[&str]) -> SimArgs {
        SimArgs {
            game: game.into(),
            agents: agents.iter().map(|&a| a.to_owned()).collect(),
            games: 50,
            seed: 7,
            threads: Some(2),
            seating: SeatingArg::Rotate,
            config: None,
            log_dir: None,
            json: false,
        }
    }

    #[test]
    fn games_lists_builtins() {
        let mut out = Vec::new();
        games(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("tictactoe"));
        assert!(text.contains("connect4"));
    }

    #[test]
    fn sim_reports_and_validates() {
        let mut out = Vec::new();
        let report = sim(&sim_args("connect4", &["random", "rules"]), &mut out).unwrap();
        assert_eq!(report.games, 50);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("rules"), "{text}");

        let mut json_args = sim_args("tictactoe", &["rules", "rules"]);
        json_args.json = true;
        json_args.config = Some(r#"{"first_seat": 1}"#.into());
        let mut out = Vec::new();
        sim(&json_args, &mut out).unwrap();
        let parsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed["games"], 50);

        let mut sink = Vec::new();
        assert!(sim(&sim_args("chess", &["random"]), &mut sink).is_err());
        assert!(sim(&sim_args("connect4", &["random", "mcts"]), &mut sink).is_err());
        let mut bad = sim_args("connect4", &["random", "random"]);
        bad.config = Some(r#"{"rows": 2}"#.into());
        assert!(sim(&bad, &mut sink).is_err());
        bad.config = Some("{".into());
        assert!(sim(&bad, &mut sink).is_err());
    }

    #[test]
    fn sim_logs_replay_with_every_view() {
        let dir = std::env::temp_dir().join(format!("ogap-cli-test-{}", GameId::new()));
        let mut args = sim_args("tictactoe", &["rules", "random"]);
        args.games = 3;
        args.log_dir = Some(dir.clone());
        sim(&args, &mut Vec::new()).unwrap();
        let files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(files.len(), 3);
        for view in ["public", "full", "seat:1"] {
            let mut out = Vec::new();
            replay(
                &ReplayArgs {
                    file: files[0].clone(),
                    view: view.into(),
                    verify: false,
                },
                &mut out,
            )
            .unwrap();
            let text = String::from_utf8(out).unwrap();
            let last: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
            assert_eq!(last["event"]["kind"], "end");
        }
        let mut out = Vec::new();
        replay(
            &ReplayArgs {
                file: files[0].clone(),
                view: "public".into(),
                verify: true,
            },
            &mut out,
        )
        .unwrap();
        assert!(String::from_utf8(out).unwrap().starts_with("ok: tictactoe"));
        for bad_view in ["seat:5", "seat:x", "everything"] {
            let args = ReplayArgs {
                file: files[0].clone(),
                view: bad_view.into(),
                verify: false,
            };
            assert!(replay(&args, &mut Vec::new()).is_err(), "{bad_view}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
