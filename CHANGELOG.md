# Changelog

All notable changes to **ACP-A2A_gateway** are documented in this file.

The project version is declared once in the root `Cargo.toml` under
`[workspace.package] version` and inherited by the `protocol`, `gateway-core`
and `gatewayd` crates via `version.workspace = true`. Bumping a release means
editing that single line (plus `Cargo.lock` on the next `cargo check`), tagging
`vX.Y.Z`, and adding an entry here. Versions follow
[Semantic Versioning](https://semver.org/).

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
