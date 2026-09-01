# Dev guide: build

> **Language:** English · [Русская версия](02-dev-guide-build-ru.md)

## Prerequisites

- Rust stable (edition 2021), `cargo` in PATH.
- For real ACP agents: the agent binary in PATH (`claurst`, `opencode`,
  etc.) — needed only for integration tests and local runs,
  not for `cargo build`.
- Not required: Docker, external services, network (the build is fully offline
  after `cargo fetch`).

## Building from scratch

```bash
git clone <repo-url> gateway && cd gateway

# Download all dependencies upfront (handy on an unstable network)
cargo fetch

# Build the entire workspace with one command
cargo build --workspace

# Release build (optimized, slower to compile)
cargo build --workspace --release
```

Binary after the build: `target/debug/gatewayd` (or `target/release/gatewayd`).

## Building individual crates

Useful when working on a single module — doesn't rebuild the whole workspace:

```bash
cargo build -p protocol   # types only, fastest build
cargo build -p core       # core + protocol
cargo build -p gatewayd   # everything (depends on core and protocol)
```

## Checking without a full build (faster in development)

```bash
cargo check --workspace       # type check without generating a binary
cargo clippy --workspace       # linter — required before commit
cargo clippy --workspace -- -D warnings   # fail on any warning (CI mode)
```

Acceptance criterion from the original spec: **`cargo check --workspace` and `clippy`
without warnings** — this must pass on every commit, not only before
release.

## First run

```bash
cp config.example.yaml config.yaml
# edit config.yaml: path to a real ACP agent, tokens,
# task_store_dir

export OPENMODEL_API_KEY="..."   # if the agent requires a key (see env: in the config)

cargo run -p gatewayd -- config.yaml
```

On a successful start, the logs (level `info`, controlled by `RUST_LOG`) show:

```
starting acp-a2a gateway (dual transport, 3 directions)
tcp transport listening (listen_addr=0.0.0.0:8347)
```

Log level control:

```bash
RUST_LOG=debug cargo run -p gatewayd -- config.yaml
RUST_LOG=core=trace,gatewayd=info cargo run -p gatewayd -- config.yaml
```

## Common build problems

| Symptom | Cause | Fix |
|---|---|---|
| `error: linking with cc failed` | No system linker (Linux) | `apt install build-essential` / `xcode-select --install` (macOS) |
| `failed to select a version for reqwest` | TLS backend version conflict | Check that the `reqwest` version is the same in `core/Cargo.toml` and `gatewayd/Cargo.toml` |
| Slow first build (>2 min) | `axum`+`reqwest`+`tokio` pull in many transitive dependencies | Normal for a first build; subsequent ones are incremental, seconds |
| `cargo clippy` fails on `unreachable!()` in `convert.rs` | This is expected — the Reply::Streaming branch is intentionally unreachable in Phase 1 | Not a bug, see the architecture guide §"seam for streaming" |

## CI minimum (if GitHub Actions or a similar setup is configured)

```yaml
# .github/workflows/ci.yml (minimal, no deploy)
steps:
  - run: cargo check --workspace
  - run: cargo clippy --workspace -- -D warnings
  - run: cargo test --workspace
```

This is exactly the same set of commands as local development — no special
CI infrastructure is needed for this project at the MVP stage.
