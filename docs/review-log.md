# Review log

Findings from `fundamentals-reviewer` gates that were accepted or deferred
(fixed findings live in git history). One line of justification each.

## Gate 1 — milestone 1 scaffold (`2a24111`) — PASS

Fixed in follow-up: default log filter (core IDs at info), grouping by
`sender_id` + message IDs, timestamps instead of preformatted strings,
grapheme/bidi-safe initials, unused deps, deterministic abort test,
redundant theme test removed.

Deferred:
- **Virtualized lists + long-message cap** → milestone 4 (chat list,
  `uniform_list`) and milestone 5 (messages, `MessageScroller` + body cap with
  a regression test). Only mock data until then.
- **Keyboard actions/focus** → milestone 2, which is exactly this scope.
- **Manual theme choice overwritten by system appearance; not persisted** →
  milestone 2 (persisted System/Light/Dark preference).
- **Unwired nav buttons, `details_open` never toggled** → milestone 2 actions.
- **Log-filter test** → not added; it would restate the constant. Revisit when
  log output tests land (security regression layer).

## Gate 2 — milestone 2 keyboard/palette/settings (`09412a0`) — PASS

Fixed in follow-up: theme selection after a startup fallback (pair built
from active themes, validated before saving, errors shown in the window);
an unparseable settings file is never overwritten (store stops saving);
save failures are shown; `ChatList::open` ignores unknown ids; shortcuts
overview lists ctrl-1..9; persistence and palette theme tests added.

Deferred:
- **Palette chat list unbounded/unvirtualized** → joins the gate 1
  virtualization deferral (milestone 4).

## Gate 3 — Helix modal keymap and composer (`2c879fd`) — PASS

Fixed in follow-up:
- `:open`/`:theme` never guess. Name matching opens only a unique match;
  look-alike or partial collisions open the chat picker instead, and an
  empty normalized query matches nothing. Peers choose their display names,
  so a guess could be steered.
- Added a "Toggle Helix keymap" palette command, so Helix mode is reachable
  from standard mode by keyboard.
- Fixed the misleading Tab-completion comment and removed its dead code.
- The composer keeps leading indentation and trims only trailing whitespace.
- Tests: every `:` command name parses; Tab cycling; Escape restores focus
  to the chat list; `:keymap standard` makes Helix bindings inert live;
  ambiguous `:open`; yank writes only the cursor's message, or nothing.
  The which-key test now covers our row builder, including unbound entries.

Deferred:
- **Jumplist advances onto a vanished chat** → milestone 4, once chats can
  disappear. Prune the entry when `ChatList::open` rejects it.
- **Copying a message is Helix-only** → milestone 5, with message hover
  actions and a context menu.

## Gate 4 — milestone 3 session layer and onboarding (`82c8843`) — PASS

Fixed in follow-up:
- **Interrupted sign-ups.** Startup loads only finished accounts; it no
  longer resumes a half-finished registration with a generated name behind
  a blank window. Sign-up resumes an unfinished registration for the same
  server (no second challenge, so a spent invitation code isn't lost) and
  then applies the chosen name. Registrations that never got past the
  challenge, or that were for another server, are deleted. The e2e test
  checks that a rejected attempt leaves no record behind.
- **Queue.** The server's version-expiry announcements are shown in the rail
  tooltip. "Unsupported version" stays shown while retrying. A sibling-state
  setup error fails the stream with a log line instead of being dropped.
- **Session.** Database changes go through a bounded channel; on overflow a
  "reload everything" flag is set. Connection state uses a watch channel.
  A profile reload replaces the previous one, so results can't land out of
  order.
- **Onboarding.** No server is contacted until the user edits the server
  field or signs up. A loading message replaces the blank screen.
- **Data dir.** Paths containing `?`, `#` or `%` are rejected before
  anything is created, since the core puts the path unescaped into a SQLite
  URL.
- **Tests.**
  - Added: `fail()` focus routing; AppRoot failure paths.
  - Merged the duplicate push-only test into the challenge test.
  - Trimmed the reconnect test's assertions on upstream's backoff schedule.
  - Fixed the e2e comment.

Deferred:
- **`Session` batching/profile-reload test** → milestone 4, when a fake or
  ephemeral `CoreUser` (upstream `test_utils`) lands with the chat list.
- **A resume after the challenge was spent** (AS registration done, QS step
  failed) isn't covered by a test; it needs a server fault injected
  mid-registration.

## Gate 5 — milestone 4 (`98ebf1f..3fc92d1`): BLOCK → fixed

Fixed:
- **Peer-controlled names and titles (high).** Group titles and display
  names arrive unvalidated (`DisplayName` TLS deserialization skips the
  length and newline checks). They now go through `model::name_line`: one
  line, invisible/bidi characters stripped, at most 64 graphemes. This
  happens where the session builds the summary, so the list, header and
  palette all get clean data. The group sender prefix and the conversation
  header title truncate. Regression test: `names_are_one_short_line`.
- **Jumplist onto a vanished chat** (gate 3 deferral). `JumpList::retain`
  drops chats that are gone before each jump. Test:
  `jump_list_skips_chats_that_are_gone`.
- **Palette size** (gate 1/2 deferral). gpui-component's `Command` renders
  through `v_virtual_list`, so the palette is already virtualized. With
  bounded titles nothing more is needed.
- **Worker stateful paths.** The e2e test now also renames the group (title
  update and system-message preview) and deletes it with notifications cut
  off, so only a full reload (`changes.all`) can notice. That covers removal
  of chats that left the order without a notification.
- **Order load failure** no longer publishes chats that have no place in
  the order, which `apply` would have dropped for good. The worker retries
  with a full reload a second later.
- **Initial load failure** retries with Fibonacci backoff instead of ending
  the worker; `LoadState::Failed` clears with the first delta.
- `chat_is_self_group` errors are logged. Removed the e2e `eprintln!`.
- **Tests.** Trimmed the pending-set test to the overflow flag surviving a
  later batch.

Deferred:
- **`Session` batching test with an ephemeral `CoreUser`.** Upstream's
  `test_utils` are `cfg(test)` inside `aircoreclient` and not exported. The
  batching is covered indirectly by the e2e test and the worker tests.
  Revisit if a fake core is introduced.
- The core's broadcast silently drops notifications on lag (it logs, but
  doesn't tell subscribers). Our overflow flag only covers our own buffer.
  An upstream issue, noted for the record.
