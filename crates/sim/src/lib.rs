//! Headless, in-process game execution for research and batch evaluation.
//!
//! No networking, no async: [`play`] drives a [`Game`] directly with
//! in-process [`Agent`]s, and [`run_batch`] runs many games in parallel with
//! rayon. Results are fully determined by the batch seed, regardless of
//! thread count, so any batch can be reproduced exactly.

use std::time::Instant;

use cogling_eventlog::{
    EventBody, Header, HeaderKind, LOG_FORMAT, LogError, LogSink, LogStore, PlayerRecord,
    SeatStats, Source, unix_ms,
};
use cogling_game_core::{
    Agent, ConfigError, ForfeitPolicy, ForfeitReason, Game, GameResult, GameRng, Rank, SeatCount,
    Seed, Setup, StateHash, Status,
};
use cogling_protocol::{CoglingInfo, CoglingInfoError, GameId};
use rayon::prelude::*;
use serde::Serialize;
use thiserror::Error;

/// Safety valve against engines that never terminate.
pub const MAX_ACTIONS_PER_GAME: u64 = 1_000_000;

/// Simulation failures (as opposed to game results such as forfeits).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SimError {
    /// The number of agents does not match the seat count.
    #[error("{agents} agents supplied for {seats} seats")]
    AgentCount {
        /// Agents supplied.
        agents: usize,
        /// Seats in the game.
        seats: usize,
    },
    /// The configuration was rejected.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Writing a log failed.
    #[error(transparent)]
    Log(#[from] LogError),
    /// An agent label is not a valid cogling name.
    #[error("invalid agent label: {0}")]
    Label(#[from] CoglingInfoError),
    /// The engine failed to serialize a value.
    #[error("engine serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    /// The thread pool could not be created.
    #[error("thread pool: {0}")]
    ThreadPool(String),
}

/// Result of one simulated game.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchRecord {
    /// Platform-level result.
    pub result: GameResult,
    /// Accepted actions.
    pub actions: u64,
}

/// Plays one game to completion.
///
/// `agents[i]` plays seat `i`. Each seat's agent gets its own deterministic
/// RNG stream derived from the setup seed. An agent that returns `None` or an
/// illegal action forfeits ([`ForfeitReason::InvalidAction`]).
///
/// If `sink` is given, every accepted action (with state hash) and the final
/// result are appended to it; the caller writes the header and closes it.
///
/// # Errors
///
/// [`SimError`] for setup, serialization or logging failures.
pub fn play<G: Game>(
    game: &G,
    setup: &Setup<G::Config>,
    agents: &mut [Box<dyn Agent<G>>],
    policy: ForfeitPolicy,
    mut sink: Option<&mut dyn LogSink>,
) -> Result<MatchRecord, SimError> {
    let seats = setup.seats();
    if agents.len() != seats.len() {
        return Err(SimError::AgentCount {
            agents: agents.len(),
            seats: seats.len(),
        });
    }
    let mut state = game.new_game(setup);
    let mut rngs: Vec<GameRng> = seats
        .seats()
        .map(|s| GameRng::from_seed(setup.seed().derive(0x5EED_0000 + u64::from(s.get()))))
        .collect();
    let mut stats: Vec<SeatStats> = seats.seats().map(SeatStats::new).collect();
    let mut actions = 0_u64;
    let started = Instant::now();
    let elapsed_ms = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let result = loop {
        let active = match game.status(&state) {
            Status::Finished(outcome) => {
                break GameResult::completed(outcome.try_map_scores(serde_json::to_value)?);
            }
            Status::ToAct(active) => active,
        };
        if actions >= MAX_ACTIONS_PER_GAME {
            break GameResult::Aborted {
                reason: format!("exceeded {MAX_ACTIONS_PER_GAME} actions"),
            };
        }
        // Simultaneous-move engines must resolve order-independently, so the
        // lowest active seat may simply go first.
        let Some(seat) = active.iter().next() else {
            break GameResult::Aborted {
                reason: "no active seat".into(),
            };
        };
        let legal = game.legal_actions(&state, seat);
        let view = game.player_view(&state, seat);
        let proposed = match (agents.get_mut(seat.index()), rngs.get_mut(seat.index())) {
            (Some(agent), Some(rng)) => agent.act(&view, &legal, rng),
            _ => None,
        };
        let Some(action) = proposed.filter(|a| game.apply(&mut state, seat, a).is_ok()) else {
            break GameResult::forfeit(
                seat,
                ForfeitReason::InvalidAction,
                seats,
                policy,
                game.provisional_ranks(&state).as_deref(),
            );
        };
        actions += 1;
        if let Some(s) = stats.get_mut(seat.index()) {
            s.record_action(0);
        }
        if let Some(sink) = sink.as_deref_mut() {
            sink.append(
                elapsed_ms(),
                EventBody::Action {
                    seat,
                    decision_id: None,
                    action: serde_json::to_value(&action)?,
                    response_ms: None,
                    state_hash: StateHash::of(&state)?,
                },
            )?;
        }
    };

    if let Some(sink) = sink {
        sink.append(
            elapsed_ms(),
            EventBody::End {
                result: result.clone(),
                stats,
            },
        )?;
    }
    Ok(MatchRecord { result, actions })
}

/// A participant in a batch: a label plus a factory producing a fresh agent
/// for each game.
pub struct Entrant<G: Game> {
    label: String,
    make: Box<dyn Fn() -> Box<dyn Agent<G>> + Send + Sync>,
}

impl<G: Game> Entrant<G> {
    /// Creates an entrant. `label` identifies it in reports and logs and must
    /// be a valid cogling name.
    ///
    /// # Errors
    ///
    /// [`CoglingInfoError`] if the label is not a valid cogling name.
    pub fn new(
        label: impl Into<String>,
        make: impl Fn() -> Box<dyn Agent<G>> + Send + Sync + 'static,
    ) -> Result<Self, CoglingInfoError> {
        let label = label.into();
        CoglingInfo::new(label.as_str(), None)?;
        Ok(Self {
            label,
            make: Box::new(make),
        })
    }

    /// The entrant's label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// How entrants are assigned to seats in each game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Seating {
    /// Game `i` seats entrant `j` at seat `(j + i) mod n`: every entrant
    /// plays every seat equally often.
    #[default]
    Rotate,
    /// A seeded random permutation per game.
    Shuffle,
}

/// Batch parameters.
#[derive(Debug, Clone)]
pub struct BatchConfig {
    /// Number of games.
    pub games: u64,
    /// Batch seed; game `i` uses `seed.derive(i)`.
    pub seed: Seed,
    /// Seat assignment policy.
    pub seating: Seating,
    /// Ranking policy for forfeits.
    pub forfeit_policy: ForfeitPolicy,
    /// Worker threads; `None` uses all cores.
    pub threads: Option<usize>,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            games: 1_000,
            seed: Seed::new(0),
            seating: Seating::default(),
            forfeit_policy: ForfeitPolicy::default(),
            threads: None,
        }
    }
}

/// Win/draw/loss counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Record {
    /// Games played.
    pub games: u64,
    /// Outright first place.
    pub wins: u64,
    /// Shared first place.
    pub draws: u64,
    /// Anything else.
    pub losses: u64,
}

impl Record {
    fn add(&mut self, rank: Rank, ranks: &[Rank]) {
        self.games += 1;
        let firsts = ranks.iter().filter(|r| **r == Rank::FIRST).count();
        match (rank == Rank::FIRST, firsts) {
            (true, 1) => self.wins += 1,
            (true, _) => self.draws += 1,
            (false, _) => self.losses += 1,
        }
    }

    /// Wins divided by games (0 if no games).
    #[must_use]
    pub fn win_rate(&self) -> f64 {
        if self.games == 0 {
            0.0
        } else {
            // Precision loss is irrelevant for counts below 2^52.
            #[allow(clippy::cast_precision_loss, reason = "game counts are far below 2^52")]
            {
                self.wins as f64 / self.games as f64
            }
        }
    }
}

/// Aggregated results for one entrant.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EntrantReport {
    /// Entrant label.
    pub label: String,
    /// Overall record (games with a result).
    pub overall: Record,
    /// Record by seat played.
    pub by_seat: Vec<Record>,
    /// Games this entrant forfeited.
    pub forfeits: u64,
    /// Sum of ranks, for average-rank computation.
    pub rank_sum: u64,
}

/// Aggregated batch results. Identical for identical inputs, independent of
/// thread count.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BatchReport {
    /// Game id.
    pub game: String,
    /// Games played.
    pub games: u64,
    /// Games without a result.
    pub aborted: u64,
    /// Accepted actions across all games.
    pub total_actions: u64,
    /// Per-entrant results, in entrant order.
    pub entrants: Vec<EntrantReport>,
}

struct GameRun {
    /// `seating[seat] = entrant index`.
    seating: Vec<usize>,
    record: MatchRecord,
}

fn seating_for(policy: Seating, n: usize, game: u64, seed: Seed) -> Vec<usize> {
    match policy {
        Seating::Rotate => {
            let shift = usize::try_from(game % n as u64).unwrap_or(0);
            // Seat s holds entrant (s - shift) mod n, i.e. entrant j sits at (j + shift) mod n.
            (0..n).map(|s| (s + n - shift) % n).collect()
        }
        Seating::Shuffle => {
            let mut order: Vec<usize> = (0..n).collect();
            GameRng::from_seed(seed.derive(0x5EA7)).shuffle(&mut order);
            order
        }
    }
}

/// Runs a batch of games between `entrants` (one per seat) in parallel.
///
/// Every game seats all entrants; [`Seating`] decides who sits where. If
/// `logs` is given, each game is written to its own log.
///
/// # Errors
///
/// [`SimError`] on setup, logging or thread-pool failure. Agent failures are
/// not errors; they are recorded as forfeits.
pub fn run_batch<G: Game>(
    game: &G,
    config: &G::Config,
    entrants: &[Entrant<G>],
    batch: &BatchConfig,
    logs: Option<&dyn LogStore>,
) -> Result<BatchReport, SimError> {
    let n = entrants.len();
    let seats = u8::try_from(n)
        .ok()
        .and_then(|n| SeatCount::new(n).ok())
        .ok_or(SimError::AgentCount {
            agents: n,
            seats: usize::from(game.info().seats.min),
        })?;
    // Validate once up front so configuration errors surface immediately.
    Setup::new(game, config.clone(), seats, batch.seed)?;

    let run_one = |i: u64| -> Result<GameRun, SimError> {
        let seed = batch.seed.derive(i);
        let setup = Setup::new(game, config.clone(), seats, seed)?;
        let seating = seating_for(batch.seating, n, i, seed);
        let mut agents: Vec<Box<dyn Agent<G>>> = seating
            .iter()
            .filter_map(|&e| entrants.get(e).map(|ent| (ent.make)()))
            .collect();
        let record = match logs {
            None => play(game, &setup, &mut agents, batch.forfeit_policy, None)?,
            Some(store) => {
                let header = sim_header(game, config, &setup, &seating, entrants)?;
                let mut sink = store.create(&header)?;
                let record = play(
                    game,
                    &setup,
                    &mut agents,
                    batch.forfeit_policy,
                    Some(sink.as_mut()),
                )?;
                sink.close()?;
                record
            }
        };
        Ok(GameRun { seating, record })
    };

    let runs: Vec<GameRun> = match batch.threads {
        None => (0..batch.games)
            .into_par_iter()
            .map(run_one)
            .collect::<Result<_, _>>()?,
        Some(threads) => rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .map_err(|e| SimError::ThreadPool(e.to_string()))?
            .install(|| {
                (0..batch.games)
                    .into_par_iter()
                    .map(run_one)
                    .collect::<Result<_, _>>()
            })?,
    };

    Ok(aggregate(game.info().id, entrants, seats, &runs))
}

fn sim_header<G: Game>(
    game: &G,
    config: &G::Config,
    setup: &Setup<G::Config>,
    seating: &[usize],
    entrants: &[Entrant<G>],
) -> Result<Header, SimError> {
    let info = game.info();
    let players = seating
        .iter()
        .zip(setup.seats().seats())
        .map(|(&e, seat)| {
            let label = entrants.get(e).map_or("unknown", |ent| ent.label.as_str());
            Ok(PlayerRecord {
                seat,
                cogling: CoglingInfo::new(label, Some("builtin"))?,
            })
        })
        .collect::<Result<Vec<_>, CoglingInfoError>>()?;
    Ok(Header {
        kind: HeaderKind::Header,
        log_format: LOG_FORMAT,
        game_id: GameId::new(),
        game: info.id.to_owned(),
        engine_version: info.engine_version.to_owned(),
        ruleset: info.ruleset.to_owned(),
        config: serde_json::to_value(config)?,
        seats: setup.seats(),
        seed: setup.seed(),
        players,
        source: Source::Sim,
        think_time_ms: None,
        observation_mode: None,
        started_at_ms: unix_ms(),
    })
}

fn aggregate<G: Game>(
    game: &str,
    entrants: &[Entrant<G>],
    seats: SeatCount,
    runs: &[GameRun],
) -> BatchReport {
    let mut report = BatchReport {
        game: game.to_owned(),
        games: runs.len() as u64,
        aborted: 0,
        total_actions: 0,
        entrants: entrants
            .iter()
            .map(|e| EntrantReport {
                label: e.label.clone(),
                overall: Record::default(),
                by_seat: vec![Record::default(); seats.len()],
                forfeits: 0,
                rank_sum: 0,
            })
            .collect(),
    };
    for run in runs {
        report.total_actions += run.record.actions;
        let Some(ranks) = run.record.result.ranks() else {
            report.aborted += 1;
            continue;
        };
        let forfeiter = match &run.record.result {
            GameResult::Forfeited { seat, .. } => Some(*seat),
            _ => None,
        };
        for (seat, (&entrant, &rank)) in seats.seats().zip(run.seating.iter().zip(ranks)) {
            let Some(er) = report.entrants.get_mut(entrant) else {
                continue;
            };
            er.overall.add(rank, ranks);
            if let Some(by_seat) = er.by_seat.get_mut(seat.index()) {
                by_seat.add(rank, ranks);
            }
            er.rank_sum += u64::from(rank.get());
            if forfeiter == Some(seat) {
                er.forfeits += 1;
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use cogling_connect4::ConnectFour;
    use cogling_eventlog::{GameLog, MemoryLogStore, Replayer};
    use cogling_game_core::{GameRegistry, RandomAgent, Seat};
    use cogling_tictactoe::TicTacToe;

    use super::*;

    fn entrants_ttt(names: &[&str]) -> Vec<Entrant<TicTacToe>> {
        names
            .iter()
            .map(|&n| {
                let owned = n.to_owned();
                Entrant::new(n, move || {
                    cogling_tictactoe::agents::by_name(&owned)
                        .unwrap_or_else(|| Box::new(RandomAgent))
                })
                .unwrap()
            })
            .collect()
    }

    fn entrants_c4(names: &[&str]) -> Vec<Entrant<ConnectFour>> {
        names
            .iter()
            .map(|&n| {
                let owned = n.to_owned();
                Entrant::new(n, move || {
                    cogling_connect4::agents::by_name(&owned)
                        .unwrap_or_else(|| Box::new(RandomAgent))
                })
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn batch_is_reproducible_across_thread_counts() {
        let entrants = entrants_c4(&["random", "rules"]);
        let mut batch = BatchConfig {
            games: 400,
            seed: Seed::new(42),
            threads: Some(1),
            ..BatchConfig::default()
        };
        let one = run_batch(&ConnectFour, &Default::default(), &entrants, &batch, None).unwrap();
        batch.threads = Some(4);
        let four = run_batch(&ConnectFour, &Default::default(), &entrants, &batch, None).unwrap();
        assert_eq!(one, four);
        batch.seed = Seed::new(43);
        let other = run_batch(&ConnectFour, &Default::default(), &entrants, &batch, None).unwrap();
        assert_ne!(one.total_actions, other.total_actions);
    }

    #[test]
    fn rotation_balances_seats_and_rules_beats_random() {
        let entrants = entrants_c4(&["random", "rules"]);
        let batch = BatchConfig {
            games: 1_000,
            seed: Seed::new(1),
            ..BatchConfig::default()
        };
        let report = run_batch(&ConnectFour, &Default::default(), &entrants, &batch, None).unwrap();
        assert_eq!(report.games, 1_000);
        assert_eq!(report.aborted, 0);
        for e in &report.entrants {
            assert_eq!(e.overall.games, 1_000);
            assert_eq!(e.by_seat[0].games, 500);
            assert_eq!(e.by_seat[1].games, 500);
            assert_eq!(e.forfeits, 0);
        }
        let random = &report.entrants[0].overall;
        let rules = &report.entrants[1].overall;
        assert_eq!(random.wins, rules.losses);
        assert!(rules.win_rate() > 0.9, "rules won only {rules:?}");
    }

    #[test]
    fn shuffle_seating_and_ttt_rules() {
        let entrants = entrants_ttt(&["rules", "random"]);
        let batch = BatchConfig {
            games: 500,
            seed: Seed::new(7),
            seating: Seating::Shuffle,
            ..BatchConfig::default()
        };
        let report = run_batch(&TicTacToe, &Default::default(), &entrants, &batch, None).unwrap();
        let rules = &report.entrants[0];
        assert!(rules.by_seat[0].games > 150 && rules.by_seat[1].games > 150);
        assert!(rules.overall.wins > 5 * rules.overall.losses, "{rules:?}");
    }

    #[test]
    fn logged_games_replay_exactly() {
        let entrants = entrants_c4(&["rules", "random"]);
        let batch = BatchConfig {
            games: 20,
            seed: Seed::new(3),
            ..BatchConfig::default()
        };
        let store = MemoryLogStore::new();
        let report = run_batch(
            &ConnectFour,
            &Default::default(),
            &entrants,
            &batch,
            Some(&store),
        )
        .unwrap();
        let logs = store.logs();
        assert_eq!(logs.len(), 20);
        let mut registry = GameRegistry::new();
        registry.register(ConnectFour).unwrap();
        let mut actions = 0;
        for bytes in logs {
            let log = GameLog::read(Cursor::new(bytes)).unwrap();
            assert_eq!(log.header().players.len(), 2);
            actions += log.actions().count() as u64;
            Replayer::new(&registry, &log)
                .unwrap()
                .run_to_end()
                .unwrap();
        }
        assert_eq!(actions, report.total_actions);
    }

    /// An agent that always proposes an illegal action.
    struct Cheater;

    impl Agent<TicTacToe> for Cheater {
        fn name(&self) -> &str {
            "cheater"
        }
        fn act(
            &mut self,
            _: &cogling_tictactoe::PlayerView,
            _: &[cogling_tictactoe::Action],
            _: &mut GameRng,
        ) -> Option<cogling_tictactoe::Action> {
            Some(cogling_tictactoe::Action {
                cell: cogling_tictactoe::Cell::new(0).ok()?,
            })
        }
    }

    #[test]
    fn illegal_agent_forfeits() {
        let setup =
            Setup::new(&TicTacToe, Default::default(), SeatCount::TWO, Seed::new(0)).unwrap();
        // Seat 0 plays cell 0 legally first; seat 1 then repeats it illegally.
        let mut agents: Vec<Box<dyn Agent<TicTacToe>>> = vec![Box::new(Cheater), Box::new(Cheater)];
        let record = play(
            &TicTacToe,
            &setup,
            &mut agents,
            ForfeitPolicy::TiedOthers,
            None,
        )
        .unwrap();
        assert_eq!(record.actions, 1);
        assert!(matches!(
            record.result,
            GameResult::Forfeited {
                seat: Seat::SECOND,
                reason: ForfeitReason::InvalidAction,
                ..
            }
        ));

        let entrants = vec![
            Entrant::new("cheater", || Box::new(Cheater) as Box<dyn Agent<TicTacToe>>).unwrap(),
            Entrant::new("random", || {
                Box::new(RandomAgent) as Box<dyn Agent<TicTacToe>>
            })
            .unwrap(),
        ];
        let report = run_batch(
            &TicTacToe,
            &Default::default(),
            &entrants,
            &BatchConfig {
                games: 10,
                ..BatchConfig::default()
            },
            None,
        )
        .unwrap();
        assert!(report.entrants[0].forfeits > 0);
    }

    #[test]
    fn errors() {
        let setup =
            Setup::new(&TicTacToe, Default::default(), SeatCount::TWO, Seed::new(0)).unwrap();
        let mut one: Vec<Box<dyn Agent<TicTacToe>>> = vec![Box::new(RandomAgent)];
        assert!(matches!(
            play(
                &TicTacToe,
                &setup,
                &mut one,
                ForfeitPolicy::TiedOthers,
                None
            ),
            Err(SimError::AgentCount {
                agents: 1,
                seats: 2
            })
        ));
        let three = entrants_ttt(&["random", "random", "random"]);
        assert!(matches!(
            run_batch(
                &TicTacToe,
                &Default::default(),
                &three,
                &BatchConfig::default(),
                None
            ),
            Err(SimError::Config(_))
        ));
        assert!(Entrant::<TicTacToe>::new("", || Box::new(RandomAgent)).is_err());
    }

    #[test]
    fn seating_rotation() {
        assert_eq!(seating_for(Seating::Rotate, 3, 0, Seed::new(0)), [0, 1, 2]);
        // Game 1: entrant 0 moves to seat 1.
        assert_eq!(seating_for(Seating::Rotate, 3, 1, Seed::new(0)), [2, 0, 1]);
        let mut s = seating_for(Seating::Shuffle, 5, 0, Seed::new(9));
        s.sort_unstable();
        assert_eq!(s, [0, 1, 2, 3, 4]);
    }
}
