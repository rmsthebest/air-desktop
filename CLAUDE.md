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
  - re-sync the `[patch]` sections in `Cargo.toml` with Air's root `Cargo.toml`;
  - bump the optional `airserver_test_harness` and `airbackend` dependencies
    to the same rev (only the `seam` feature pulls them in: the harness turns
    on the core's `test_utils`, which changes behaviour, so other builds, the e2e tests
    included, keep the production core).
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
    `VersionStatus` is the server's word on this version, kept apart from
    the connection status: only a listen it accepts ends a refusal.
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
    `o` on a reply jumps to its original (`Messages::jump_to_message`:
    only a message of this chat; the window is replaced around it when it
    isn't loaded; only what then shows counts as read), a click on the
    quote too; it scrolls by index, as a replaced window has no heights
    yet. Messages of only 1–5 emoji on one line (≤ 256 bytes) show large
    (`model::is_jumbo_emoji`), drawn from the source, so its bound matters.
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
    `model::expires_label` shows an open request's countdown from the
    core's `contact_request_expires_at`, redrawn every 20 s.
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
    `:contact-member <name|#tag>` sends a member a contact request through
    the group (`add_contact_from_group`, confirmed with name and tag,
    post-quantum off as upstream's default); an existing contact or an
    open request of the user's is refused before the core (which refuses
    them only as internal errors). The new chat opens once listed.
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
    message is in the open chat. `shift-r` sends the user's failed upload
    again (`retry_upload`, as upstream: already uploaded is left to the
    outbound service's recovery; held by the same `IN_FLIGHT` claim; a
    gone original removes the message). `v` views an image: the
    downloaded original decoded once more, under larger limits (4096²
    edge, 80 MiB, 32 MiB file; kept ≤ 2048²), in a dialog whose closing
    drops the texture. An image the core says moves also decodes its
    original (GIF, WebP, APNG; ≤ 8 MiB) by `decode_animation`: 1024² canvas,
    240 frames, 32 MiB of frames, delays under 20 ms slowed to 100 ms; past
    any limit it stays still. After each update is sent, the worker gives
    the window's 6 newest such images the slots (`fill_animations`; a failed
    decode holds one too, older ones lose theirs). gpui plays the frames
    and redraws at display rate meanwhile, whatever the delays: the view
    swaps to the still preview after 3 loops or 30 s
    (`limit_animation_loops` setting, on by default), and `p` gives it its
    loops again.
    Sending: `:attach <path>`, the file
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
    One rule table, `model::ContactOp::check`, on a chat's `OneToOne`
    facts (side, stage, whether the peer is known; derived by
    `chats::one_to_one`): the workspace asks it before the dialog (and
    only picks the wording), the session again on the core's state.
    Blocking is local and synced to the user's devices: a contact's
    chat shows blocked (no composer, an Unblock footer; no preview,
    badge or unread stop in the list) and the core drops their
    messages. Blocking an incoming request declines and erases it: with
    no blocked-contacts list (the core has none public, nor has
    upstream), that can't be undone, and the dialog says so. In a
    group, `:report <member>` (name or `#tag`, like `:kick`), and
    `:delete-chat` deletes the group. Deleting a contact ends their
    chat too (the dialog says so); a contact's ended chat has no
    composer, only a footer to delete it.
  - `message_ops.rs`: deleting a message and its edit history (upstream
    `ChatDetailsCubit`). One rule table, `model::DeleteFor::check`, on
    `MessageFacts`/`ChatFacts`, asked by the view before the dialog and by
    the session again on the core's state: the core's delete for
    everyone doesn't check the sender, so only the session stops a peer's
    message being replaced. For everyone: own, sent, not deleted, not the
    self chat, a chat that takes messages. For me: anything the core
    erases (some system messages stay; `erasable` mirrors the core's
    private `is_deletable`). Keys on the message cursor: `d`/`delete` for
    me, `shift-d` for everyone (each confirmed), `i` details (Helix:
    `space k`, as `i` inserts). Details show sender, time, own status and
    the edit history (newest first, at most 20 versions, rendered as
    messages are). Both run locally (a deletion for everyone is queued),
    so failures never blame the connection (`Failure::local`).
  - Mute (`chats.rs` `Session::set_muted`): `:mute [30m|8h|2d|1w|tomorrow|
    monday|forever]` (empty: forever; `model::Mute::parse`), `:unmute`,
    `secondary-shift-m` toggles. This device only, as upstream; alerts
    already skip muted chats. The core stores forever as 9999-01-01 and
    reads it back as a date: `chats::mute` maps it to forever. The list
    tags muted chats and redraws when a timed mute ends.
  - `account_settings.rs`: the synced account settings (read receipts,
    deleting expired contact requests), reloaded when the core reports
    their keys changed (also its rollbacks); `:read-receipts on|off`,
    `:delete-expired-requests on|off`, and settings rows.
  - `invitations.rs`: invite codes (upstream `InvitationCodesCubit`):
    codes, then tokens to trade for one, reloaded when the settings open
    (tokens arrive untold). Codes are shown and copied reduced to letters
    and digits (`model::invite_code`); copying marks them given out.
  - Deleting the account (`Session::delete_account`, upstream
    `UserCubit::delete_account`): `:delete-account delete` (the typed
    word, as upstream) then a confirm dialog. The core stores the
    deletion first and finishes it in the background if it takes long
    (Pending: told so). Completed, the unlink watcher reports
    `account_deleted` and the app root resets the device. An abort the
    user wasn't told (in the background, or while closed) is reported
    once (`DeletionAborted`) and recorded, checked at start and on own
    user changes; not while this session's own deletion runs (it tells
    its outcome).
  - `safety.rs`: safety codes, port of upstream `UserCubit::safety_codes`.
    The core hashes each user's credential; the pair is ordered so both
    sides see the same 12 zero-padded five-digit blocks (3 lines of 4).
    `:safety-code` (`secondary-shift-s`) for the open contact, or
    `:safety-code <member>` (name or `#tag`) in a group; in a one-to-one
    chat only once its request is accepted (a name there is refused).
    A late code for a chat the user left is dropped; a failed lookup (often a
    peer credential not here yet) never blames the connection. Shown in a dialog:
    Enter copies, Escape closes.
  - `avatars.rs`: profile and group pictures. A chat shows its group's
    picture or an established contact's (`model::shows_picture`: not a
    requester's, a blocked one's, or the self chat's); members and the
    nav rail show profile pictures. Air sends them as JPEG the core shrank
    to 256²: only JPEG is drawn, decoded by us under limits (512 KiB,
    1024² edge, 8 MiB alloc), kept at most 128². Workers keep each
    picture with a keyed hash of its bytes (`Avatars`), so a reload with
    the same bytes reuses it; entities drop a replaced picture's texture
    (`drop_unused`). Setting one (`:picture [path]`, `:group-picture
    [path]`, no path: file dialog; `:remove-picture`,
    `:remove-group-picture`; a settings row) first decodes the file
    once under limits (`read_source`, 4096² edge): the core resizes it
    without limits, and for a group silently drops one it can't read,
    which would remove the current picture. Profile updates cost the
    server's per-address rate limit one request per group: the e2e
    retries them slowly (`patiently`).
- `src/about.rs`, `src/logs.rs`, `src/ui/info_view.rs` — about, licenses, logs
  and debug info (`:about`, `:licenses`, `:logs`, `:debug-info`, a settings
  row), in place of the settings rows. `Cargo.toml`'s `repository` is the
  source and issue link, shown as "not published yet" while empty: the AGPL
  has us offer the source, so set it before a release. `just
  regenerate-licenses` (needs cargo-about) rewrites `assets/licenses.json`
  after `Cargo.lock` changes; a test fails until then. Logs stay in memory
  (1000 lines, one per event, cleared on a device reset), as they can name
  contacts; stdout gets them only on a terminal or with `RUST_LOG` set.
- `src/username.rs` — port of upstream's suggestion, prefilled for a first
  username only. The server refusing this version replaces the workspace
  (`Workspace::update_required`, in `app_root`); an announced end shows a banner
  above the chat list until dismissed (a later date shows it again).
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
- `src/emoji.rs`, `src/ui/emoji_picker.rs` — emoji by name (`emojis`
  crate data, GitHub shortcodes). A closed `:shortcode:` in the composer
  becomes its emoji as typed (only at a word start, so `10:30:` stays);
  the picker (gpui-component `Command`, like the palette) inserts one
  (`secondary-;`, palette "Insert emoji") or reacts with any (`/` in the
  reaction bar).
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
  and `:leave`/`:delete-chat` behind a confirm dialog (`ui::confirm` closes
  the dialog and restores focus itself before running: gpui-component's OK
  path doesn't; `Workspace::confirm` reaches the workspace through a weak
  handle).
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
  core opens or creates them, and locked (`flock`) so a second instance is
  refused with a message (`paths.rs`).

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
- `just seam` — the account seam tests (ignored by default): see Testing.
- `just release-check` — builds the package from HEAD's tree as a tagged
  tarball would be (`packaging/aur/`). `depends` comes from `readelf -d` and
  what a running process maps: Vulkan and Wayland are loaded at runtime.
- `just release-ready` — before tagging: fails while the source link or a
  placeholder in the package files is missing.
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
--ignored` (and `e2e_account`: synced settings, invite codes, account deletion), and the two-user test with `AIR_E2E_CODES=<code>,<code> cargo test
e2e_two -- --ignored` (also pictures); linked devices with `AIR_E2E_CODES=<code>,<code> cargo test e2e_linked --
--ignored`; blocking, reporting, retracting and deleting (contacts and messages, edit history, mute) with three codes, `AIR_E2E_CODES=<code>,<code>,<code>
cargo test e2e_contacts -- --ignored`; a contact request through a group with three codes, `e2e_contact_from_group`; add `AIR_E2E_DATA_DIR=<dir>` to keep the account, then
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
tests for keyboard/focus flows, account seam tests, ignored e2e tests against
a local server, security regression tests. Every test must fail for a
plausible regression; don't test upstream crypto/MLS or gpui-component
internals.

Account seam tests (`just seam`: the `seam` feature, ignored tests) run the real core
against Air's in-process test server (`airserver_test_harness`): no
invitation codes, no rate limits, fault injection through
`listener_control_handle()`, and helpers to connect users, send files and
build groups. They need only Postgres and MinIO from Air's compose file (not
`run-server`). Session-level ones are tokio tests in `src/session/seam.rs`;
workspace ones (`src/ui/workspace/tests/seam.rs`) run a real `Session` in a
`#[gpui::test]` with parking allowed, polling in real time (`Seam::until`),
and app-root ones are in `ui/app_root.rs`. The backend is dropped inside
the tokio runtime (a kept handle, never through the app: a drop may run while
a panic unwinds). Test-only accessors they need are
`cfg(all(test, feature = "seam"))`; `just check` lints with the feature.
Count toasts with `ui::take_told` (all of them), and before asserting that
something was dropped, assert it started (`act_then_open`'s `started`). `window.dispatch_action` is deferred to the end of the
update while `ChatList::open` selects at once: to act, then switch chats
before a result lands, use separate updates (`Seam::act_then_open`). Seed a
user's own messages with `outbound_service().run_once()` after each send: one
waiting for the service moves when the server stamps it. A
loaded (not freshly made) account would look for its server by domain, so
`AppRoot::with_account` starts on a harness user directly.
