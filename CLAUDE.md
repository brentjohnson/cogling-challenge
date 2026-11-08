# CLAUDE.md

Guidance for Claude Code when working in this repository.

## Project overview

Cogling Challenge: the board game bot battle. You build it. It plays. Developers write bots, called coglings, in any language; they connect over WebSocket and compete in turn-based board games with no humans at the table.

- **Terminology:** call them coglings everywhere (docs, code, protocol). "Bot" appears only in the tagline and the one-line definition. `Agent` is a separate concept: an in-process player used by the simulator.
- **Kind:** workspace (libraries + the `cogling` binary in `crates/cli`)
- **MSRV:** 1.95 — keep in sync with `rust-version` in the workspace Cargo.toml
- **Edition:** 2024

## Commands

Run these from the repository root.

| Task | Command |
|------|---------|
| Build | `cargo build --all-targets` |
| Test (all) | `cargo test --all-features` |
| Test (one) | `cargo test <name> -- --nocapture` |
| Format | `cargo fmt --all` |
| Format check | `cargo fmt --all -- --check` |
| Lint | `cargo clippy --all-targets --all-features -- -D warnings` |
| Docs | `cargo doc --no-deps --all-features` |
| Doc tests | `cargo test --doc` |
| Dependency audit | `cargo deny check` (or `cargo audit`) |

## Definition of done

Before reporting a task as finished, all of these must pass with no warnings:

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test --all-features`

New behavior ships with tests. Bug fixes ship with a regression test that fails before the fix.
If something can't be made to pass, say so explicitly — never weaken a test or add `#[allow(...)]` just to get green.

## Code style

### Error handling
- No `.unwrap()` outside tests. Use `?` to propagate.
- `.expect("why this cannot fail")` only for true invariants, with a comment explaining the invariant.
- Libraries: a structured error enum per module/crate using `thiserror`. Don't expose `anyhow::Error` in public APIs.
- Binaries: `anyhow::Result` with `.context("what we were doing")` at boundaries.
- No `panic!`, `todo!`, or `unimplemented!` in shipped code paths.

### Types and API design
- Parse, don't validate: convert raw input into types that can only hold valid values (newtypes, enums, constructors returning `Result`).
- Make invalid states unrepresentable — prefer enums over boolean flags and `Option` soup; use type-state where transitions matter.
- Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/): `as_`/`to_`/`into_` naming, implement common traits (`Debug`, `Clone`, `PartialEq`, `Default`, `Display`) where they make sense, accept `impl AsRef<str>` / `&[T]` / `impl IntoIterator` rather than concrete owned types.
- Keep `pub` surface minimal; default to `pub(crate)`.
- Mark public enums/structs that may grow with `#[non_exhaustive]`.
- Keep `impl` blocks next to their type definitions. No `types.rs` / `impls.rs` splits.

### Ownership and performance
- Prefer borrowing (`&T`, `&mut T`, `&str`, `&[T]`) over `.clone()` / `.to_owned()`. Clone only when ownership is genuinely needed.
- Use `Vec::with_capacity` / `String::with_capacity` when the size is known or estimable.
- Use `Rc`/`Arc`/`Mutex`/`RefCell` only when shared ownership or interior mutability is actually required.
- Prefer iterators over index loops; avoid collecting into intermediate `Vec`s that are immediately consumed.
- Don't optimize speculatively — measure with `cargo bench` / `criterion` first.

### Unsafe
- Avoid `unsafe`. If unavoidable, isolate it in a small module, add a `// SAFETY:` comment on every block explaining why the invariants hold, and cover it with tests (run under `cargo miri test` where possible).

### Documentation
- Every public item has a `///` doc comment. Include an `# Examples` section for non-trivial APIs (doc tests are compiled and run).
- Document `# Errors` and `# Panics` sections where applicable.
- Comments explain *why*, not *what*.

## Testing

- Unit tests live in a `#[cfg(test)] mod tests` at the bottom of the file they test.
- Integration tests live in `tests/` and exercise only the public API.
- `.unwrap()` is fine in tests; prefer returning `Result<(), Box<dyn std::error::Error>>` when it reads better.
- Test error paths, not just the happy path.
- Consider `proptest` for parsers and anything with a large input space.

## Dependencies

- Ask before adding a new dependency. Prefer the standard library when it's reasonable.
- Prefer well-maintained, widely used crates; disable default features you don't need.
- Don't bump MSRV or major dependency versions without calling it out.

## Project layout

<!-- Fill in once the structure exists, e.g.:
- `src/lib.rs` — public API and module declarations
- `src/main.rs` — CLI entry point (thin: parse args, call into lib, report errors)
- `tests/` — integration tests
- `benches/` — criterion benchmarks
-->

Keep `main.rs` thin: argument parsing and error reporting only, with logic in the library so it's testable.

## Things to avoid

- Don't commit or push unless asked.
- Don't edit generated files or `Cargo.lock` by hand.
- Don't silence lints crate-wide; scope any `#[allow]` to the smallest item and add a reason (`#[allow(clippy::foo, reason = "...")]` or `#[expect(...)]`).
- Don't leave `dbg!`, `println!` debugging, or commented-out code behind.
