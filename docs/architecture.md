# Architecture

A modular monolith: one binary, clear crate boundaries, no infrastructure
prerequisites. Dependencies point one way:

```
games/* ──▶ game-core ◀── protocol ◀── eventlog ◀── sim
                 ▲            ▲            ▲
                 └────────── server ───────┘ ◀── bot-sdk (tests) , cli
```

- **Engines** (`game-core`, `games/*`) are pure: no IO, async, clocks or
  ambient randomness. All randomness comes from `GameRng`, whose algorithms
  are frozen because replays depend on them. Its state lives inside the game
  state. Every engine passes `game_core::conformance::check`.
- **Server** runs one tokio task per game (an actor that owns the
  authoritative state), so game state has no locks. Connections send it
  commands and receive messages through per-connection outboxes. Deadlines
  and grace periods are decided in the actor using frame-arrival timestamps.
- **Simulation** calls engines directly (no WebSocket, no tokio) and
  parallelizes across games with rayon. Results are aggregated in game order,
  so batches are reproducible regardless of thread count.
- **Event logs** are JSONL: a header (game, exact engine version, ruleset,
  canonical config, seats, seed, players) plus numbered events. Each accepted
  action carries the resulting state hash, so replays are verified step by
  step. Logs are omniscient. Public and per-seat views are derived on replay.

## Decisions

| # | Decision |
|---|---|
| A1 | **Forfeit in 3+ player games ends the game.** The forfeiter ranks last. The others tie (default) or are ranked by `Game::provisional_ranks` (`ForfeitPolicy::Provisional`). Engines never need to remove a seat mid-game. |
| A2 | Think time is measured by the server from `decide` sent to action received, so network latency counts. |
| A3 | A disconnected seat keeps its clock running. It forfeits at whichever comes first: the decision deadline or the end of the reconnect grace period. |
| A4 | Compound turns with large action spaces (e.g. placing or buying several things in one turn) are modelled as sequences of small atomic actions (`place X` … `done`), keeping legal-action lists small. |
| A5 | `Status::ToAct` is a non-empty seat set, which permits simultaneous moves. Such engines must resolve them order-independently. |
| A6 | One connection plays one game at a time, and may play several games one after another. |
| A7 | **Hosted competitions use a pull pool**: idle connected bot instances are formed into tables, prioritising bots with the fewest games played toward a per-bot target, then the least-seen opponent pairs and seats. *(Phase 4.)* |
| A8 | Replays require the exact engine version and ruleset recorded in the log. |
| A9 | Full logs are served only after a game ends. Spectators get public views. |
| A10 | Raw ranks and game-specific scores are stored. Ratings (Weng-Lin/OpenSkill style) are derived and recomputable. *(Phase 4.)* |
| A11 | Standalone mode supports lobbies (`create_game` + `join`) and quick-match queues. |

## Deliberate deviations from the initial plan

- `rand`/`rand_chacha` are not used for game randomness. Their sampling
  helpers may change between versions, which would break historical replays.
  `GameRng` (SplitMix64 + xoshiro256** + Lemire) is frozen and tested against
  reference vectors. `rand` is used only for server seeds and resume tokens.
- Unknown JSON fields are ignored rather than rejected, for forward
  compatibility within a protocol version. Unknown message types are still
  rejected.
- Timestamps are Unix milliseconds via std, so there is no date-time
  dependency.
- Periodic state snapshots in logs are not implemented yet. Replay from the
  start is fast for the current games. Snapshots can be added as a new event
  kind when a game needs them.

## Not yet built (next phases)

- **Phase 4, hosted:** accounts, API keys (stored as SHA-256 of 256-bit
  secrets), PostgreSQL via `sqlx` with migrations, a database-backed
  `LogStore`, `join_competition`, the pull-pool scheduler, statistics and
  ratings. The server already exposes the seams these need: `LogStore`, the
  `Hub` matchmaking entry points, and `ServerConfig` limits.
- **Phase 5:** log export for ML, version comparison tooling.
- **Phase 6:** more games, including multi-player games with hidden
  information and multi-step turns. Each is a pure engine crate that passes
  the conformance kit; the platform needs no changes per game.
