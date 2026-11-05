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
  line, invisible/bidi characters stripped, at most 64 graphemes (see
  gate 5b for the byte bound). This
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
  off, so only a full reload (`changes.all`) can notice. It catches the
  deletion through the "chat no longer loads" path. (Corrected at gate 5b:
  this first said it covered chats leaving the order, which it doesn't.)
- **Order load failure** no longer publishes chats that have no place in
  the order, which `apply` would have dropped for good. The worker retries
  with a full reload a second later.
- **Initial load failure** retries with Fibonacci backoff instead of ending
  the worker; `LoadState::Failed` clears with the first delta.
- `chat_is_self_group` errors are logged. Removed the e2e `eprintln!`.
- **Tests.** Trimmed the pending-set test to the overflow flag surviving a
  later batch.

Deferred:
- **`Session` batching test with an ephemeral `CoreUser`.** `aircoreclient`
  has a `test_utils` feature that could enable this as a dev-dependency
  feature. It changes core internals in test builds, so it gets its own
  step: milestone 5, where message paging needs a local core anyway. Until
  then the batching is covered indirectly by the e2e test and the worker
  tests.
- The core's broadcast silently drops notifications on lag (it logs, but
  doesn't tell subscribers). Our overflow flag only covers our own buffer.
  An upstream issue, noted for the record.

## Gate 5b — gate 5 fixes (`3fc92d1..7f11979`): PASS

Fixed anyway:
- **Size bound (medium).** The grapheme cap didn't bound bytes: one cluster
  can stack thousands of combining marks. Each grapheme now keeps at most 8
  chars (room for ZWJ emoji), so names are at most 64×8 chars and previews
  160×8.
- **Order retry (medium).** It reloaded every chat once a second, forever,
  even when nothing was held back. Now it retries only when a chat new to
  the list was actually held back, as an order-only reload (it finds the
  chat through "a chat no notification told us about"), with Fibonacci
  backoff that resets on success. Waiting doesn't block other changes.
- **Blank names.** Added Hangul fillers, U+061C, U+034F, U+180E and tag
  characters to the stripped set. A name left empty becomes "Unnamed".
- **Reserved labels.** Preview prefixes are typed (`Draft`, `You`,
  `Sender`); the client's labels render italic in the accent colour, so a
  peer named "You" can't pass for the local user. The self chat gets a
  "Self" tag, so a peer group titled "Notes to self" can't pass for it.
- **Jumplist.** Pruning now collapses a chat that ends up next to itself.
- **Details pane.** The title truncates like everywhere else.
- **Tests.** The order bookkeeping moved into a pure
  `LocalState::reconcile`. New tests cover the held-back-then-placed path
  and removal by order-diff (`a_failed_order_holds_new_chats_back_until_one_loads`,
  `chats_leave_when_gone_or_out_of_the_order`), plus zalgo, blank-name, a
  peer named "You" and the duplicate jumplist cases. Corrected the gate 5
  e2e coverage claim.

## Gate 6 — milestone 5, conversation (`834d137..d341300`): BLOCK

Fixed:
- **Mark as read never fired (high).** It relied on
  `ListState::is_scrolled_to_end`, which is `None` for short chats and
  while items are unmeasured, and on the scroll handler, which only fires
  for wheel scrolls. Now every drawn message records its bounds, and a probe
  after the list works out what is in view each frame. A change pages in
  more and marks the newest visible message seen if the window is active,
  as does window activation. Tests:
  `only_messages_shown_in_an_active_window_are_seen` and
  `a_chat_opens_at_its_unread_divider`.
- **Quadratic highlight flattening (medium).** Styled runs are built
  disjoint, sorted and merged while parsing, with links folded in as a style,
  so drawing is linear. Test: `styled_runs_stay_disjoint_and_linear_in_the_input`.
- **Link confirmation could hide the host (medium).** Targets are parsed and
  normalized with `url`: punycode hosts, escaped paths, ASCII only. What
  opens is exactly what the dialog shows. Web links with a user name are
  refused. The dialog leads with the host, keeping its end, then the whole
  target, shortened in the middle. Test: `link_labels_show_where_links_really_go`.
- **Didn't open at the divider (medium).** The view scrolls the divider to
  the top once the first messages arrive. Pages appended while newer
  messages are still unloaded pin the view instead of following the tail.
  Only what is in view is marked.
- **Cursor drifted across splices (medium).** The cursor shifts with each
  splice, and drops if its message goes. Test:
  `the_cursor_stays_on_its_message_across_splices`.
- **Paging requests piled up (medium).** One request per direction is in
  flight at a time; the flag clears on the next window update.
- **Merged Add|Update dropped (medium).** Any change to a message in the
  window reloads it, Add included.
- **Lows.**
  - An empty heading no longer turns the next paragraph into one.
  - Closing a chat within the debounce still marks what was seen, on drop.
  - Equal timestamps are told apart by id.
  - `push` asserts it's only used on fixed messages.
  - The initial load retries with backoff.

Deferred:
- **Worker `changed()` paths without a server** (Add|Update on a loaded
  message, an own message clearing the divider, a sender rename). These need
  an ephemeral `CoreUser` via the core `test_utils` feature. Milestone 6 adds
  sending, which drives Add/Update of own messages end to end, so this goes
  in with it, together with the `Session` batching test from gate 5.
- **A Replace keeps the index, not the message, in view** (overflow reload
  in place). Rare (a lagging broadcast). Revisit with the e2e for sending.
