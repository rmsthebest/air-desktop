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
  GPUI executors. Exception: core operations whose failure path cleans up
  (contact requests) run detached (`Tokio::handle(cx).spawn`), so closing
  a view can't stop them midway.

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
  - `chats.rs`: port of `ChatsDataSource`. A tokio worker owns what was
    published (`LocalState`), reloads only chats a change touches (directly,
    via their last message, or via a user whose name they show) and sends
    deltas; `Chats` entity applies them. Session changes reach the worker
    through a merged `Pending` set, so bursts coalesce.
  - `preview.rs`: the chat-list preview line (draft > last message), names
    and system-message text resolved by the core. Peer text goes through
    `model::preview_line` (one line, invisible chars stripped, bounded).
  - `messages.rs`: port of `MessageListCubit`. A tokio worker owns a
    bounded window (`MAX_WINDOW`) of one chat's messages, pages it on
    request, follows changes, and sends `Change`s the entity applies and
    re-emits as list splices. Reactions come with each message (the core
    loads them) and are summarized by `model::reactions`: peer emoji are
    unchecked text, so each is cut to one visible grapheme, look-alikes
    merge (each reactor counted once per label), at most 12 kinds show,
    the user's own always; alerts use the same label.
    `set_reaction` adds or retracts as the view decided (detached, like
    sends; the core ignores a repeated add). The picker
    (`+`, then 1–6) is a bar under the list, not part of a list item, so
    opening it doesn't re-measure the list. Opens at the first unread message. Marking as
    read is debounced, only for messages shown while the window is active,
    and sends receipts as upstream (self chat only when receipts are off).
  - `composer.rs`: port of `ChatDetailsCubit`'s draft/send parts. The
    composer's text is the view's; the draft is what was last stored from
    it (debounced 1 s, committed on leaving the chat) plus the edit or
    reply. Pure `DraftState` transitions; stores and sends run serialized
    on tokio, detached so they finish after the chat closes. A send clears
    only the draft version it sent; a refused edit becomes a new message.
  - `contacts.rs`: port of upstream `UsernameContext` (one listener per
    own username, reconciled on `DbEntityId::Username` changes; failed
    requests acked unless `keep_failed_username_message`) plus add
    contact / accept / decline / add & remove username. Refusals are
    `Failure::Shown` (told to the user, not logged: they name contacts);
    internal errors are logged and reported generically. A new account's
    privacy-pass tokens arrive in the background, so the first username
    or contact request can fail and succeed on retry.
  - `pending.rs`: the merged change set both workers wake on.
  - `groups.rs`: create / invite / remove / rename / leave / delete, run
    detached like contact operations, and the `Members` entity (an open
    group's members, reloaded on its chat's or any user's change;
    `can_remove` is upstream's `can_kick` on the room policy, which for
    the core's trusted default lets any member remove another; ended
    groups refuse everything but delete). Contacts and members are picked
    by name with `matches_name` (never guessed); members also by `#tag`,
    a hash prefix of the user id grown until unique in the group, so a
    copied name can't shield anyone from removal. A query is a tag only
    when all of it is `#<hex>`, so names can't smuggle one in; completion
    inserts a name only if it finds that member again, else the tag; and
    `:kick` confirms with the resolved name and tag. Delete ends a group for
    every member and erases it on all the user's devices (the dialog says
    so).
    The self chat is a group in the core but never one here
    (`is_group` false, operations refuse it). Invites need the contact's
    `encrypted_group_profiles` feature, as upstream.
  - `attachments.rs`: port of upstream's `attachments_repository`. The
    session's `download_all` downloads pending attachments (5 at once,
    resumed after restarts) and makes an image's thumbnail in the core,
    then reports the attachment changed itself (the core doesn't notify
    for thumbnails). Images are drawn only from the core's thumbnail, or
    the original where the core says it fits (≤ 8 MiB), decoded by us as
    WebP under limits (1024² edge, 16 MiB) into a gpui `RenderImage`:
    never a failed thumbnail's original, never undecoded bytes. The
    message worker caches decoded pictures per attachment (kept at most
    at twice the drawn box, at most 64 per window); the `Messages` entity
    drops a picture's GPU texture when it leaves the window. The core
    doesn't notify when a download's content lands: the downloader
    reports it. After an upload the message must be enqueued with the
    outbound service (a failed one marked failed), as upstream. One
    download per attachment at a time (`IN_FLIGHT`).
    Files are never opened: `s` on one saves a copy to the downloads
    directory under `model::file_name` (one component, not hidden,
    bounded), never replacing a file (`create_new`, numbered), 0600.
    Save and download-now (`s` on a failed or waiting file) check the
    message is in the open chat. Sending: `:attach <path>`, the file
    dialog (`AttachFile`) or dropping files on the chat (confirmed
    first); several files go one after another.
  - `alerts.rs`: desktop notifications ("alerts"; database notifications
    are the core's). Port of upstream `notifications/` + `NotificationPolicy`:
    the queue and username listeners hand over what processing stored; at
    most one alert per chat, its text from the core's notification rebuild
    set (newest unread, unnotified entry from others), so edits and remote
    deletes rewrite (`Notice::Refresh`) or dismiss a shown alert.
    Sibling-suppressed chats are marked notified (so dismissed); muted
    chats skipped. `Attention` (window active, open chat, `notifications`
    setting) decides what is shown and the suppression reported to the
    user's other devices (re-reported when it changes). The workspace
    reports attention on chat open/close and window activation.
  - `devices.rs`: port of upstream `LinkedDevicesCubit` + `multi_device.rs`.
    `Devices` entity: one row per self-group member, named from the synced
    `LinkedDevicesSetting` (bounded lines), reloaded on that setting or the
    self chat; `#tag` is a client-id prefix grown until unique (names are
    often alike). Rename any device, unlink any other (confirmed with name
    and tag; not revocation: it deletes its own data once it processes the
    removal). Linking from this device (`:link <code>`, `Session::link_device`)
    runs detached; dropping the `Link` before `confirm` turns it down, after
    it the core finishes or undoes it. Linking this device at onboarding
    (`provision`) is cancelled through its token, never aborted: the core
    deletes the half-made database. The QR code carries upstream's
    `air://<domain>/multiDeviceLinkingCode?code=…` URL. `unlinked` watches
    the own user for the core's unlink reason; the app root then drops the
    workspace, closes and deletes this client's database (as upstream's
    `deleteCurrentDatabase`) and shows onboarding with a notice.
  - `contact_safety.rs`: block / unblock, report spam, retract an
    outgoing request, delete a one-to-one chat (upstream `UserCubit`).
    The pure `check` decides from the chat's type and status what may
    run and on whom (a request sent to a username has no known peer);
    the workspace's `contact_confirmation` words the dialog and refuses
    early, but the session's check is the one that counts. Blocking is
    local and synced to the user's devices: an incoming request is
    declined, a contact's chat shows `blocked` (no composer, an Unblock
    footer, no preview or badge in the list) and the core drops their
    messages. In a group, `:report <member>` (name or `#tag`, like
    `:kick`) and `:delete-chat` deletes the group. `:delete-chat` refuses
    open requests (decline or retract them).
  - `safety.rs`: safety codes, port of upstream `UserCubit::safety_codes`.
    The core hashes each user's credential; the pair is ordered so both
    sides see the same 12 zero-padded five-digit blocks (3 lines of 4).
    `:safety-code` (`secondary-shift-s`) for the open contact, or
    `:safety-code <member>` (name or `#tag`) in a group; in a one-to-one
    chat only once its request is accepted (a name there is refused).
    A late code for a chat the user left is dropped; a failed lookup (often a
    peer credential not here yet) never blames the connection. Shown in a dialog:
    Enter copies, Escape closes.
- `src/notifier.rs` — shows alerts via `org.freedesktop.Notifications`
  (zbus 5 on async-io, gpui's own copy). Replaces a chat's notification
  instead of stacking, closes it on dismissal, escapes bodies when the
  server renders markup. No-op off Linux. Notices travel in order through
  `alerts::NoticeQueue`, which keeps each chat's latest notices (bounded by
  the chats, nothing dropped). A failed close is retried before each
  later notice; a rewrite that gets a new id (the user closed the old one)
  is closed at once. The notifier re-applies `Attention`
  at show time. The D-Bus side sits behind the `notifier::Desktop` trait
  (tests use a fake screen). The server's signals are heard only from the
  instance that gave out our ids: a click (`default` action) opens the
  chat (`OpenFromAlert`, the workspace raises the window; gpui can't use
  the server's Wayland activation token), a user dismissal advances each
  shown chat's `notified_until` to the alert's `until` (what it showed,
  never newer), and a closed notification is forgotten so no rewrite
  brings it back. `private` mode keeps one shared notification for all
  chats, closed once none needs it. While the server can't be told, no
  id is replaced or closed; failed closes retry every 30 s. Signals are
  read by a pump task (unread ones would stall zbus's reader). A close
  that follows a click marks nothing. The fake screen can't catch wire
  types: `notify_args`' signature is pinned, and
  `cargo test desktop_notify_and_close -- --ignored` uses the real server.
- The local server rate-limits sign-ups: back-to-back e2e runs fail with
  "Rate limited"/"Too Many Requests" at sign-up or the first requests.
- `src/markdown.rs` — bounded message markdown parse (size/depth caps; only
  http/https/mailto become links; images and HTML stay inert text).
  `ui/markdown_view.rs` renders it; links always open through a confirm
  dialog showing the real target.
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
- Keys that act only sometimes (up = edit last message in an empty
  composer, escape = drop a reply/edit) bind in `Composer > Input` and call
  `cx.propagate()` otherwise: dispatch then tries the next matching binding
  (the input's own).
- A chat with an open contact request has no composer (`render_request`
  takes its place); focusing the composer there focuses the messages.
- Contacts are added from the command line (`:contact <username>`, which
  `AddContact` prefills); a new chat opens once the list has it
  (`ChatList::open_when_listed`). Groups likewise: `:group <name>`
  (`NewGroup`), `:invite`/`:kick` (complete contacts/members), `:rename`,
  and `:leave`/`:delete-chat` behind a confirm dialog (`Workspace::confirm`
  reaches the workspace through a weak handle, and closes the dialog and
  restores focus itself before running: gpui-component's OK path doesn't).
- Normal-mode letter bindings must never live in a context that contains a
  text input (they'd steal typing): use `ChatList`/`MessageList`, or
  `Workspace && helix && !Input`.
- `src/settings.rs` — `<config>/settings.json` (`SettingsStore` global).
- `src/ui/link_view.rs` — linking a new device from this one, shown in place
  of the settings rows; the workspace runs the link and asks before
  confirming (full account access). Hiding the settings turns an
  unconfirmed link down; a confirmed one reports its outcome as a toast.
  Onboarding's link mode (`secondary-l`, Escape back) shows the code
  (`ui/qr.rs`, dark on white with a quiet zone).
- `src/ui/settings_view.rs` — the settings screen (`OpenSettings`,
  `ctrl-,` / `space s`), shown in place of the conversation. A list of
  rows: up/down (j/k) pick, left/right (h/l, enter) step a choice; it
  only emits `SettingsEvent`s, which the workspace carries out the way
  the matching command does. Delete removes the selected username or
  unlinks the selected device; enter on a device prefills
  `:device-name #<tag> <name>`. Text (display name, a new username) is
  typed on the command line, prefilled (`:name`, `:username`); removing
  a username is confirmed. Themes step within their mode and keep the
  appearance (`theme::set_theme`). Opening a chat or focusing the
  conversation hides it.
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

## Roadmap
`docs/roadmap.md` lists the milestones after 7, in order.

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
Clients use domain `localhost`. E2E: `AIR_E2E_CODE=<code> cargo test e2e_sign_up --
--ignored`, and the two-user test with `AIR_E2E_CODES=<code>,<code> cargo test
e2e_two -- --ignored`; linked devices with `AIR_E2E_CODES=<code>,<code> cargo test e2e_linked --
--ignored`; blocking, reporting, retracting and deleting with `AIR_E2E_CODES=<code>,<code>
cargo test e2e_contacts -- --ignored`; add `AIR_E2E_DATA_DIR=<dir>` to keep the account, then
`AIR_DESKTOP_DATA_DIR=<dir> cargo run`. E2E tests use `flavor = "multi_thread"`: the
core calls `block_in_place`.

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
