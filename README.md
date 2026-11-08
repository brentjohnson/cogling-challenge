# Open Game Agent Platform (`ogap`)

An open platform where developers build autonomous bots that compete in
turn-based games. Bots run anywhere, in any language, and connect over a
documented WebSocket/JSON protocol. Game engines are pure Rust libraries that
also run headless for large reproducible simulations.

**Status:** standalone platform (milestones M0–M7). Tic-Tac-Toe and Connect
Four are the reference games. The hosted competition platform comes next (see
[docs/architecture.md](docs/architecture.md)).

## Quick start

```sh
cargo build --release
alias ogap=./target/release/ogap

ogap games                                    # list games
ogap serve                                    # local server on ws://127.0.0.1:7878/ws, logs in ./logs

# in two more terminals: a Rust bot and a Python bot play each other
ogap bot --game connect4 --strategy rules --games 5
python3 clients/python/random_bot.py --game connect4 --games 5

ogap replay logs/connect4-<id>.jsonl --verify # re-simulate and verify a game
ogap replay logs/connect4-<id>.jsonl --view seat:1   # states as seat 1 saw them
```

Headless batch simulation (no network, reproducible from the seed regardless
of thread count):

```sh
ogap sim --game connect4 --agents random,rules --games 20000 --seed 42
ogap sim --game tictactoe --agents rules,random --games 1000 --log-dir sim-logs --json
```

Lobby games:

```sh
id=$(ogap game create --game tictactoe --think-time-ms 500)
ogap bot --game tictactoe --join "$id" &
python3 clients/python/rules_bot.py --join "$id"
```

## Writing a bot

- **Any language:** implement [docs/protocol.md](docs/protocol.md), which is
  plain JSON over a WebSocket.
- **Rust:** implement `ogap_bot_sdk::Bot::decide` and call `ogap_bot_sdk::run`
  (see `crates/bot-sdk/examples/random_bot.rs`).
- **Python:** subclass `ogap_client.Bot` (standard library only; see
  `clients/python`).

## Layout

| Path | What |
|---|---|
| `crates/game-core` | `Game` trait, outcomes, frozen deterministic RNG, type-erased registry, conformance test kit |
| `crates/games/{tictactoe,connect4}` | Reference engines and rule-based agents |
| `crates/protocol` | Versioned bot protocol messages and JSON Schema |
| `crates/eventlog` | JSONL game logs, log stores, verified replay |
| `crates/sim` | Headless match runner and parallel batches |
| `crates/server` | Session actors, deadlines, lobbies, quick-match, WebSocket transport |
| `crates/bot-sdk` | Rust client SDK and example strategies |
| `crates/cli` | The `ogap` binary |
| `clients/python` | Dependency-free Python client and sample bots |
| `docs/` | Protocol spec, schema, architecture |

## Development

```sh
./scripts/check.sh                       # fmt check + clippy -D warnings + all tests
(cd clients/python && python3 -m unittest -v)
UPDATE_SCHEMA=1 cargo test -p ogap-protocol   # regenerate docs/protocol.schema.json
```
