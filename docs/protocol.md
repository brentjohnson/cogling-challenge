# Cogling Protocol — version 1

Coglings connect to the game server over a **WebSocket** and exchange **JSON text
frames**. Each frame holds one message object with a `"type"` field. The
protocol is the same in standalone and hosted mode. Hosted mode additionally
requires an `api_key` in `hello`.

- Endpoint: `ws://<host>:<port>/ws` (standalone default `ws://127.0.0.1:7878/ws`)
- Machine-readable schema: [`protocol.schema.json`](protocol.schema.json)
  (generated from the Rust types; a test fails if it drifts)
- Reference clients: Rust [`crates/sdk`](../crates/sdk), Python
  [`clients/python`](../clients/python)

## Principles

- **The server is authoritative.** Coglings send *actions*, the decisions a human
  player could make. They never send state changes.
- **Observations are self-contained.** Every `decide` carries the cogling's full
  player-specific view, so coglings never need to track deltas. Views never contain
  information the seat is not entitled to.
- **One connection, one game at a time.** Games are never multiplexed over a
  connection. After `game_over` the connection may queue or join again. Run
  several cogling processes to play several games at once.
- **Forward compatibility.** Unknown *fields* are ignored. Unknown message
  *types* are rejected with `error: malformed`.

## Lifecycle

```
cogling                                   server
 │── hello ─────────────────────────────▶│
 │◀───────────────────────────── welcome ─│
 │── queue {game} ──────────────────────▶│   or: join {game_id} (lobby)
 │◀────────────────────────────── queued ─│       ◀── joined
 │◀────────────────── game_start {seat} ──│   (once all seats are filled)
 │◀──────── decide {decision_id, …} ──────│   decision clock starts
 │── action {decision_id, action} ──────▶│
 │◀────────────────── action_accepted ────│   or action_rejected → retry
 │        … repeat decide/action …        │
 │◀───────────────── game_over {result} ──│
 │── queue … (next game) or close         │
```

## Messages: cogling → server

| type | fields | notes |
|---|---|---|
| `hello` | `protocol` (must be `1`), `cogling: {name, version?}`, `api_key?` | Must be the first message, within 10 s. `name`: 1–64 chars, no control characters or surrounding whitespace. `version`: 1–32 chars from `[A-Za-z0-9.+_-]`. |
| `queue` | `game`, `seats?`, `config?` | Quick-match. Coglings are matched only with coglings asking for the same game, seat count and *canonical* config. Seats are assigned randomly. |
| `leave_queue` | — | |
| `create_game` | `game`, `seats?`, `config?`, `think_time_ms?`, `observation_mode?` | Creates a lobby. Reply: `game_created {game_id}`. Does not join it. |
| `join` | `game_id` | Takes the next free seat in a lobby (seats are filled in join order). The game starts when full. |
| `action` | `game_id`, `decision_id`, `action` | `action` is game-specific JSON (see below). |
| `reconnect` | `game_id`, `resume_token` | Reclaims your seat after a disconnect (see below). |
| `ping` | `nonce?` | Reply: `pong {nonce}`. |

## Messages: server → cogling

| type | fields | notes |
|---|---|---|
| `welcome` | `protocol`, `server_version`, `limits: {max_message_bytes, max_messages_per_second}` | |
| `queued` | `game` | |
| `game_created` | `game_id` | |
| `joined` | `game_id`, `seats_filled`, `seats_total` | Sent to everyone in the lobby on each join. |
| `game_start` | `game_id`, `game`, `engine_version`, `ruleset`, `config`, `seat`, `players: [{seat, cogling}]`, `resume_token`, `observation_mode`, `think_time_ms` | `config` is the canonical config in effect. Keep `resume_token` secret. |
| `decide` | `game_id`, `decision_id`, `remaining_ms`, `observation`, `legal_actions?` | Your turn. `legal_actions` only in `state_and_legal` mode. |
| `action_accepted` | `game_id`, `decision_id` | |
| `action_rejected` | `game_id`, `decision_id`, `code`, `message`, `remaining_ms` | Retry with the same `decision_id` while time remains. |
| `game_over` | `game_id`, `result`, `observation` | Your final observation (`null` if the game never started). |
| `error` | `code`, `message`, `fatal` | If `fatal`, the server closes the connection. |
| `pong` | `nonce?` | |

### Results

```json
{"kind": "completed", "ranks": [1, 2], "scores": { "...game-specific..." }}
{"kind": "forfeited", "seat": 0, "reason": "timeout" | "disconnected", "ranks": [2, 1]}
{"kind": "aborted",   "reason": "lobby expired"}
```

`ranks[i]` is seat *i*'s finishing position (1 = best). Ties share a rank
using standard competition ranking (`1, 2, 2, 4`). A draw ranks every seat 1.
In a forfeit the forfeiting seat ranks last and the game ends immediately for
everyone. By default the other seats share first place. Servers may instead
rank them by the engine's provisional standing (`--forfeit-policy provisional`).

## Decisions, deadlines and retries

- `decision_id` increases monotonically within a game. Every `decide` has a
  new one, and actions must echo it.
- The decision clock starts when `decide` is sent and is measured **by the
  server**, so network latency counts. Default think time is 1000 ms. Games
  and servers can configure 10 ms–10 min.
- An invalid action gets `action_rejected` with a descriptive `message`. **The
  clock is not reset.** You may retry with the same `decision_id` until the
  deadline.
- An action with a stale or unknown `decision_id` is rejected as
  `stale_decision`. It is not applied and does not count as an invalid move.
- If the deadline passes, the seat **forfeits** (`reason: "timeout"`). The
  server never invents moves.

Reject codes: `illegal_action`, `malformed_action`, `not_your_turn`,
`stale_decision`, `game_finished`.

## Disconnects and reconnects

- If the connection drops, the seat stays yours and the decision clock keeps
  running.
- Open a new connection, send `hello`, then
  `reconnect {game_id, resume_token}`. The server re-sends `game_start` (same
  seat, same token) and, if a decision is pending, re-sends that `decide` with
  the time actually remaining.
- Unless you reconnect within the grace period (default 10 s), the seat
  forfeits (`reason: "disconnected"`). If a decision is pending, its deadline
  usually comes first.
- A newer connection that reclaims the seat supersedes the old one. The old
  one receives a fatal `error: superseded`.

## Observation modes

Set per game (`create_game`) or per server (`--observation-mode`):

- `state`: `decide` carries the observation only.
- `state_and_legal` (default): it also carries `legal_actions`, the complete
  list of legal actions. Sending one of them verbatim is always valid.

## Errors and limits

Error codes: `protocol_version`, `malformed`, `unexpected_message`,
`hello_required`, `auth_required`, `unknown_game`, `no_such_game`, `game_full`,
`invalid_config`, `invalid_resume`, `superseded`, `rate_limited`,
`message_too_large`, `server_busy`, `internal`.

- Frames larger than `max_message_bytes` (default 64 KiB) close the connection.
- Messages beyond `max_messages_per_second` (default 100, bursts of 2×) get
  `error: rate_limited` and are dropped.
- Malformed or rate-limited messages are counted. After too many (default 10),
  the connection is closed with a fatal error.
- Binary frames are not supported.

## Game reference

### `tictactoe` (seats: 2)

- Config: `{"first_seat": 0}`. Seat 0 plays `X`, seat 1 plays `O`.
- Action: `{"cell": n}` with cells numbered `0..8` row-major.
- Observation: `{"board": [9 × "X"|"O"|null], "seat", "mark", "to_move"}`.
- Scores: `{"winning_line": [a, b, c] | null}`.

### `connect4` (seats: 2)

- Config: `{"first_seat": 0, "columns": 7, "rows": 6}`. Each dimension may be 4–12.
- Action: `{"column": c}`.
- Observation: `{"board": {"columns", "rows", "cells"}, "seat", "to_move"}`.
  `cells` is flat and row-major starting at the **bottom** row:
  `cells[row * columns + column]` is a seat number or `null`.
- Scores: `{"kind": "win", "seat", "line": [{column,row} × 4]}` or `{"kind": "draw"}`.

## Example session

```json
→ {"type":"hello","protocol":1,"cogling":{"name":"Strategist","version":"2.3.1"}}
← {"type":"welcome","protocol":1,"server_version":"0.1.0","limits":{"max_message_bytes":65536,"max_messages_per_second":100}}
→ {"type":"queue","game":"tictactoe"}
← {"type":"queued","game":"tictactoe"}
← {"type":"game_start","game_id":"01a1…","game":"tictactoe","engine_version":"0.1.0","ruleset":"standard","config":{"first_seat":0},"seat":0,"players":[…],"resume_token":"…","observation_mode":"state_and_legal","think_time_ms":1000}
← {"type":"decide","game_id":"01a1…","decision_id":1,"remaining_ms":1000,"observation":{"board":[null,…],"seat":0,"mark":"X","to_move":0},"legal_actions":[{"cell":0},…]}
→ {"type":"action","game_id":"01a1…","decision_id":1,"action":{"cell":4}}
← {"type":"action_accepted","game_id":"01a1…","decision_id":1}
…
← {"type":"game_over","game_id":"01a1…","result":{"kind":"completed","ranks":[1,2],"scores":{"winning_line":[0,4,8]}},"observation":{…}}
```

## Versioning

The `protocol` number changes only for incompatible changes. New optional
fields and new message types that older coglings never receive are compatible
additions. Hosted-mode messages such as `join_competition` will be added that way.
