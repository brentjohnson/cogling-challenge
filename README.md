# Cogling Challenge

**The board game bot battle. You build it. It plays.**

Write a bot, called a cogling, in any language. Connect it over WebSocket and
let it compete in turn-based board games, with no humans at the table.

Game engines are pure Rust libraries that also run headless for large
reproducible simulations.

**Status:** standalone platform (milestones M0–M7). Tic-Tac-Toe and Connect
Four are the reference games. The hosted competition platform comes next (see
[docs/architecture.md](docs/architecture.md)).

## Quick start

```sh
cargo build --release
alias cogling=./target/release/cogling       # optionally also: alias cog=cogling

cogling games                                    # list games
cogling serve                                    # local server on ws://127.0.0.1:7878/ws, logs in ./logs

# in two more terminals: a Rust cogling and a Python cogling play each other
cogling run --game connect4 --strategy rules --games 5
python3 clients/python/random_cogling.py --game connect4 --games 5

cogling replay logs/connect4-<id>.jsonl --verify # re-simulate and verify a game
cogling replay logs/connect4-<id>.jsonl --view seat:1   # states as seat 1 saw them
```

Headless batch simulation (no network, reproducible from the seed regardless
of thread count):

```sh
cogling sim --game connect4 --agents random,rules --games 20000 --seed 42
cogling sim --game tictactoe --agents rules,random --games 1000 --log-dir sim-logs --json
```

Lobby games:

```sh
id=$(cogling game create --game tictactoe --think-time-ms 500)
cogling run --game tictactoe --join "$id" &
python3 clients/python/rules_cogling.py --join "$id"
```

## Writing a cogling

- **Any language:** implement [docs/protocol.md](docs/protocol.md), which is
  plain JSON over a WebSocket.
- **Rust:** implement `cogling_sdk::Cogling::decide` and call `cogling_sdk::run`
  (see `crates/sdk/examples/random_cogling.rs`).
- **Python:** subclass `cogling_client.Cogling` (standard library only; see
  `clients/python`).

## Layout

| Path | What |
|---|---|
| `crates/game-core` | `Game` trait, outcomes, frozen deterministic RNG, type-erased registry, conformance test kit |
| `crates/games/{tictactoe,connect4}` | Reference engines and rule-based agents |
| `crates/protocol` | Versioned cogling protocol messages and JSON Schema |
| `crates/eventlog` | JSONL game logs, log stores, verified replay |
| `crates/sim` | Headless match runner and parallel batches |
| `crates/server` | Session actors, deadlines, lobbies, quick-match, WebSocket transport |
| `crates/sdk` | Rust client SDK and example strategies |
| `crates/cli` | The `cogling` binary |
| `clients/python` | Dependency-free Python client and sample coglings |
| `docs/` | Protocol spec, schema, architecture |

## Development

```sh
./scripts/check.sh                       # fmt check + clippy -D warnings + all tests
(cd clients/python && python3 -m unittest -v)
UPDATE_SCHEMA=1 cargo test -p cogling-protocol   # regenerate docs/protocol.schema.json
```

## License

[MIT](LICENSE)
