# air-desktop

Keyboard-first GPUI desktop client for the Air messenger (github.com/phnx-im/air).
License: AGPL-3.0-or-later (links Air's AGPL crates).

## Stack
- UI: `gpui-kit =0.9.0` (pins `gpui-pre =0.3.11` + gpui-component). Never add
  another gpui lineage (crates.io `gpui`, zed git) — trait mismatches follow.
- Core: `aircoreclient`/`aircommon`/`airapiclient` as git deps pinned to one Air
  rev. When bumping the rev:
  - copy Air's `Cargo.lock` as a starting point (newer transitive versions
    break the build, e.g. `rsa` for `blind-rsa-signatures`), then re-resolve;
  - re-sync the `[patch]` sections in `Cargo.toml` with Air's root `Cargo.toml`.
- `.cargo/config.toml` sets `SQLX_OFFLINE=true` (Air compiles sqlx queries from
  checked-in metadata).
- Async: Air's core needs tokio. Run it via `runtime::Tokio::spawn`, which
  returns a `TokioTask` (aborts on drop). Never poll tokio futures directly on
  GPUI executors.

## Layout
- `src/model.rs` — plain view-model types the UI renders (+ `mock` data until
  the session layer lands).
- `src/session/` — ports of upstream `applogic` as GPUI entities. Views never
  touch `aircoreclient` directly.
  - `account.rs`: load default account / sign up (invitation codes only:
    desktop has no push token, so no admission sessions).
  - `queue.rs`: QS listener with upstream's reconnect/backoff; processing
    the queue's Empty sentinel is what starts the core's outbound service.
  - `Session` entity: owns `CoreUser`, runs the listener, forwards
    `db_notifications` to the UI as batched `SessionEvent::Changed`.
- `src/ui/app_root.rs` — window root: loading → onboarding → workspace.
- `src/ui/` — views. Render state, dispatch actions; no business logic.
- `src/theme.rs`, `assets/themes/*.json` — gpui-component JSON themes; user
  themes from `<config>/themes/*.json`, read at startup (gpui-component's
  watcher drops string-loaded themes on reload, so it isn't used).
- `src/actions.rs`, `assets/keymaps/default.json`, `src/keymap.rs` — every
  command is an `air::` action; defaults and `<config>/keymap.json` share one
  loader (`null` unbinds; bad entries are warnings, never fatal).
- Helix mode (opt-in, `"keymap_mode": "helix"` or `:keymap helix`):
  `assets/keymaps/helix.json` is always bound, but its contexts require the
  `helix` identifier that `ui::key_context` adds only in Helix mode — so
  switching modes needs no rebinding. Mode is derived from focus: composer
  focused = INS, otherwise NOR. `:` commands are parsed in
  `src/command_line.rs` (pure); which-key uses `possible_bindings_for_input`.
- Normal-mode letter bindings must never live in a context that contains a
  text input (they'd steal typing): use `ChatList`/`MessageList`, or
  `Workspace && helix && !Input`.
- `src/settings.rs` — `<config>/settings.json` (`SettingsStore` global).
- Config dir: XDG (`~/.config/air-desktop`), override with
  `AIR_DESKTOP_CONFIG_DIR`. Data dir (account DBs): XDG data dir, override
  with `AIR_DESKTOP_DATA_DIR`; kept 0700, files tightened to 0600 after the
  core opens or creates them (`paths.rs`).

## GPUI gotchas seen here
- Dialogs (palette) render outside the workspace tree: actions dispatched
  from inside never reach workspace handlers. Close the dialog, then dispatch.
- An action handler runs inside its entity's update: don't `read` that
  entity again from code it calls (panics). Pass data in instead.
- Entity events are delivered after the current update, not synchronously.

## Commands
- `just check` — fmt check, clippy `-D warnings`, tests.
- `cargo run` — run the app.
- `gpui` is an alias for `gpui_kit` (`extern crate` in main.rs) so GPUI
  derive macros resolve; tests use `#[gpui::test]`.
  Test support comes from `gpui-pre`/`gpui-pre-platform` dev-deps, not
  gpui-kit's `test-support` feature (it pulls a conflicting backend).

## Local backend (Podman, no Docker)
From an Air checkout:
1. `podman compose up -d` (Postgres + MinIO). If the short name
   `postgres:18.3` doesn't resolve, use `docker.io/library/postgres:18.3` in a
   local override.
2. `SKIP_DOCKER_COMPOSE=true just run-server` → `http://localhost:8080`.
3. `cargo run --bin airserver -- code generate 2` for invitation codes
   (the local server requires them).
Clients use domain `localhost`. E2E: `AIR_E2E_CODE=<code> cargo test e2e --
--ignored`; add `AIR_E2E_DATA_DIR=<dir>` to keep the account, then
`AIR_DESKTOP_DATA_DIR=<dir> cargo run`.

## Review gate
After every milestone commit (and any commit touching session/crypto/storage or
rendering of untrusted content) run the `fundamentals-reviewer` agent in a fresh
context with the commit range since the last gate. Give it only the range — not
the implementer's conclusions. Fix BLOCK findings before the next milestone;
record accepted/deferred findings in `docs/review-log.md`.

## Testing principles
Few layers, each with a purpose: model unit tests, `#[gpui::test]` behaviour
tests for keyboard/focus flows, ignored e2e tests against a local server,
security regression tests. Every test must fail for a plausible regression;
don't test upstream crypto/MLS or gpui-component internals.
