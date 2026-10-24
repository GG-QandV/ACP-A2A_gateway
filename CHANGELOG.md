# Changelog

All notable changes to **ACP-A2A_gateway** are documented in this file.

The project version is declared once in the root `Cargo.toml` under
`[workspace.package] version` and inherited by the `protocol`, `gateway-core`
and `gatewayd` crates via `version.workspace = true`. Bumping a release means
editing that single line (plus `Cargo.lock` on the next `cargo check`), tagging
`vX.Y.Z`, and adding an entry here. Versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- `.github/workflows/ci.yml`: `cargo fmt --all --check` + `cargo clippy --workspace
  --all-targets -- -D warnings` + `cargo test --workspace --locked` on ubuntu, windows and
  macOS runners (first green run: all three platforms).
- `scripts/publish-docs.sh`: publishes `AGENTS.md` / `CLAUDE.md` into `main` from the
  out-of-repo overlay, then re-applies `skip-worktree` so indexing-tool noise stays invisible.

### Added
- Non-blocking `message/send` / `SendMessage`: with `params.configuration.blocking: false` the
  gateway answers at once with the task in state `working` and runs the turn in the background;
  `tasks/get` serves the final task with the agent's text, `tasks/resubscribe` the events. An
  absent flag keeps the old blocking behavior.

### Fixed
- SSE responses (`message/send` streams and `tasks/resubscribe`) now send a `: keep-alive`
  comment every 15 s while the agent is silent **between events**. The response headers still
  wait for the agent's first event, so a proxy that cuts a response silent for ~30 s before the
  first event is handled by non-blocking send (above), not by keep-alive.
- The last agent chunk was dropped at random when the final response arrived together with it
  (unbiased `select!` in the stdio stream relay); queued chunks now always precede the terminal.

### Changed
- The whole workspace was formatted with `cargo fmt --all` — the repository had never passed
  `cargo fmt --check` before (22 files), which would have failed the new Format gate at once.
- `AGENTS.md` and `CLAUDE.md` are no longer tracked in the repository; their vetted versions
  come from `_doc_overlay/ACP-A2A_gateway/`. Git-ignored locally: `/AGENTS.md`, `/CLAUDE.md`,
  `/_LOCAL_RULE_gitnexus-docs.md`.

## [1.1.2] - 2026-09-01

### Added
- Single source of truth for the project version: `[workspace.package]`
  (`version`, `edition`) in the root `Cargo.toml`.
- This changelog; the version is now stated in `README.md`.

### Changed
- Documentation is now English-primary: every guide/spec/log has an English file
  under its plain name and a Russian copy under the `-ru` suffix
  (e.g. `docs/02-dev-guide-build.md` + `docs/02-dev-guide-build-ru.md`).
  Previously Russian lived in the unsuffixed file (the `06-gateway-guide`,
  `A2A-protocol-strategy-2026` and `INSTALL` families were flipped accordingly).
- `docs/TZ-*` / `docs/tz-*` renamed to `docs/SPEC-*`; `docs/шлюз-описание-a2a.md`
  to `docs/a2a-gateway-overview.md` — no Cyrillic in tracked file names.
- Decision identifiers are now Latin (`P-18`) in both languages.
- `protocol`, `core` and `gatewayd` manifests use
  `version.workspace = true` / `edition.workspace = true` instead of repeating
  the version.
- README: English is the primary document, the Russian version stays local-only.
- `gatewayd`: register the `hermes` agent (ACP) with a 90 s streaming
  first-chunk timeout.

### Removed
- Working documents are no longer tracked and are covered by `.gitignore`:
  `docs/Правки/`, `docs/правки 2/`, `docs/правки 3+аддс/`, `docs/правки 4/`,
  `docs/суть+маркетинг/`, `docs/ToDo/`, `docs/bags/`.

## [1.0.2] - 2026-08-19

### Added
- README and architecture diagram (SVG).
- Durable event storage, journal CLI and agent approvals (phases 1-7).
- Part 4 logging: gzip rotation, prune, `/debug/level` runtime reload, T10 load
  test.
- Resubscribe with event-log replay (`get-last-seq` + SSE replay).
