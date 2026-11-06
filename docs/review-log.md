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

## Gate 6b — gate 6 fixes (`d341300..8e54447`): PASS

Fixed anyway:
- **Partly visible counted as seen (medium).** Paging still looks at every
  message partly in view, but only the newest one in view in full (or
  filling the view) is marked as read. Test:
  `only_messages_in_view_in_full_count_as_seen`.
- **Retried layout passes.** Probes keep the last bounds per message, so a
  retried pass can't widen what counts as shown (same test).
- **Read state with equal timestamps.** Ordered by time, then id, as the
  core orders messages, so it never moves back. Test:
  `what_was_seen_never_moves_back`.
- **Empty window.** Paging older from an empty window (everything loaded
  was deleted) reloads the bottom.
- **mailto with extra recipients.** Refused (`to`/`cc`/`bcc` query keys),
  since they'd hide behind the address shown.

Accepted:
- **Paging flags clear on any update**, not only on the page that answers.
  At worst one extra page is fetched; bounded.

Deferred (with milestone 6, needs an ephemeral `CoreUser`):
- A view test that a paged append pins what's shown.
- Worker tests: the Add|Update reload, initial-load backoff, the flush on
  drop, and the in-flight flags.

## Gate 7 — milestone 6a, composer (`e922f26..2c45bba`): PASS

Fixed anyway:
- **Draft lost on quick back-and-forth (medium).** Each composer had its own
  lock, and leaving a chat stored its draft 300 ms late. Reopening the chat
  in between could load nothing, then store an empty draft over the real
  one. Now one `DraftQueue` per session runs every draft load, store and
  send in the order the UI queued them, and leaving stores right away (the
  delay is gone). Reopening a chat therefore always reads what leaving it
  stored. An empty composer with no draft stores nothing.
- **Drafts never committed after quitting (medium).** The queue first runs
  `commit_all_message_drafts`, as upstream does at startup. Accepted: text
  typed less than a second before quitting isn't stored (as upstream on a
  kill).
- **Reply/edit reused while a send was in flight (low).** A send now takes
  the draft out of memory. A failure gives it back (a refused edit as a new
  message) unless a newer draft was started. Test:
  `a_send_takes_the_draft_and_a_failure_gives_it_back` (replaces the
  setter-only refused-edit test, as suggested).
- **`e` dropped typed text (low).** It refuses with a notice while the
  composer holds text.
- **Quotes from other chats (low).** The core resolves a reply's original by
  MIMI id alone. A quote now shows only if the original is in the same
  chat; otherwise it reads "Message unavailable". Worth raising upstream.
- **Stale quotes (low).** Replies reload when their original changes. The
  core quotes the version replied to, so edits don't change a quote, but
  deletion does ("Deleted message"). The e2e test covers it, plus clearing a
  draft that came from the database on send.

Corrected:
- The core's `test_utils` feature doesn't provide an offline `CoreUser`
  (only a server URL override), so the gate 6/6b worker tests can't run
  without a server. Covered by e2e so far: Add|Update (own send then status
  and edit patches), the divider-clearing own send, and reply refresh.
  Still open, for the two-user e2e in 6b: a sender rename re-patching
  messages, the in-flight paging flags, flush on drop, initial-load
  backoff, the `Session` batching test, and the paged-append view test.

Deferred:
- View tests that a loaded draft only fills an empty composer and that a
  failed send restores text only into a blank one. Both need a live
  composer; they go with the two-user e2e harness.

## Gate 7b — gate 7 fixes (`2c45bba..3131d76`): PASS

Fixed anyway:
- **Failed send re-attached to new text (medium).** "Started a new
  message" now means the same for state and view: text typed since the
  send, stored or not. A failure then doesn't bring back its reply/edit.
  Covered in `a_send_takes_the_draft_and_a_failure_gives_it_back`.
- **Edit of a deleted message stuck in edit mode (medium).** A missing
  original is refused like a passed edit window: the text comes back as a
  new message.
- **Failure after leaving the chat was lost (low).** It is logged outside
  the entity, and the text is kept as the chat's draft (through the queue,
  unless the chat has a newer draft).
- **Replies whose original arrives later (low).** They keep the awaited MIMI
  id and reload when a message with it arrives.
- **Errors reloading replies are logged (low).**
- **`e` while editing (low).** Switching from one edit to another is
  allowed.
- **Tests.** `a_quote_of_a_deleted_message_says_so`. The e2e test queues a
  draft store then a load and checks the load sees the store. Caveat: that
  only shows the queue works. A regression to unqueued loads would fail it
  only on unlucky timing.

## Gate 8 — milestone 6b, contacts (`c072511..d5a4c21`): BLOCK

Fixed:
- **Taken username reported as "already yours" (high).** The core's
  `add_username` also returns `None` when the server says the name belongs to
  someone else. A local check now decides "already yours"; otherwise the
  message is "taken". Covered in the two-user e2e test: Bob claims Ada's name.
- **Ended requests shown as open (medium).** An inactive connection chat is
  now `Request::Closed`, shown as "ended", with no Accept or Decline
  (`an_ended_contact_request_is_closed_not_open`). Deleting such a chat is
  deferred.
- **Unicode case folding loosened usernames (low).** Input is folded with
  ASCII rules only; the KELVIN SIGN case is now in the test.
- **Second contact operation silently dropped (low).** The user is now told
  that the previous request is still running.
- **Contact operations aborted with the window (low).** The tokio task is
  no longer aborted when its caller is dropped, so the core's cleanup
  always runs.
- **Tests.** `request_keys_leave_a_normal_chat_alone`: ctrl-enter still
  sends from a normal chat's composer. It fails if accept doesn't
  propagate.

Deferred:
- **The request banner leads with the peer-chosen display name (low).**
  Upstream does the same, and the chat list's "Request" tag marks it. A
  later version could show which of the user's usernames the request came
  through.
- **Still open from gate 7:** in-flight paging flags, flush on drop,
  initial-load backoff, `Session` batching, the paged-append view test.
  The two-user e2e now covers read receipts and a sender rename re-patch.

## Gate 8b — gate 8 fixes (`d5a4c21..97b5fd0`): PASS

Fixed anyway (all low):
- A request from a blocked peer is `Closed`, so it offers no Accept
  (assert added to `an_ended_contact_request_is_closed_not_open`).
- The chat list tags an ended request "Ended", not "Request".
- CLAUDE.md names detached tokio tasks for core operations with cleanup as
  the exception to `Tokio::spawn`. The comment says such a task survives the
  view, not an app quit.

## Gate 9 — milestone 7, desktop notifications (`34cc83d..5ccebf5`): PASS

Fixed:
- **Deleted or edited text stayed on screen (medium).** Chats in
  `chats_with_changed_notifications` are now checked like the user's own
  sibling-sent messages: no unread, unnotified entry left means the alert
  is dismissed. The two-user e2e test has Ada delete a message and checks
  that Bob's alert for it is dismissed.
- **Dismissals could be dropped (low).** They have their own unbounded
  channel. The notifier handles them first. Covered in
  `a_full_queue_still_takes_dismissals`.
- **A stricter setting left revealing alerts up (low).** Moving to
  `private` or `off` dismisses everything shown. The pure `alerts::attend`
  rule is covered in `looking_at_a_chat_or_revealing_less_dismisses`.
- **An error note hid a burst (low).** A chat's alert comes from its newest
  message that can be put into words. A chat counts as alerting only once
  an alert was actually built.
- **An alert could land right after opening its chat (low).** The notifier
  drops alerts for the chat being watched at show time.
- **No bus meant one warning per alert (low).** The notifier warns once and
  retries at most once a minute. D-Bus calls time out after 5 s.

Deferred:
- **Private mode keeps one notification per chat (low).** Their count shows
  how many chats are active. One shared slot would fix it, but then a
  dismissal could close a different chat's alert.
- **Re-reporting the suppression has no dedicated test.** The arm in
  `queue::listen_once` only compares two values. A useful test needs a
  sibling device, which the e2e setup doesn't have yet.

## Gate 9b — gate 9 fixes (`5ccebf5..59cd278`): PASS

Gate 9 marked "deleted or edited text stayed on screen" as fixed. That fix
held only when nothing else in the chat was unread. Fixed now:
- **Deleted or edited text stayed up beside other unread messages
  (medium).** Alert text now comes from the chat's notification rebuild
  set, not from the processed event. A chat that changed quietly gets its
  alert rewritten from what is left (`Notice::Refresh`; ignored if nothing
  is shown) or dismissed. An alerting chat is built from the set too, so a
  send and a delete in one batch don't show the deleted text. The two-user
  e2e test deletes a message while an older one is unread (the alert falls
  back to it), and again after reading (the alert is dismissed).
- **Dismissals could overtake older alerts (medium).** Alerts and
  dismissals share the ordered `NoticeQueue`. Alerts are capped. A
  dismissal is always taken and drops the queued notices it makes stale.
  The notifier re-applies the attention (content and watched chat) at
  show time. Covered in
  `dismissals_are_kept_in_order_and_drop_what_they_make_stale`, and
  `what_is_shown_follows_the_latest_notices_and_setting` (the notifier
  against a fake screen).
- **Sibling-suppressed chats kept their alert (low).** They are marked
  notified and checked like quiet chats, so their alert is dismissed.
- **CLAUDE.md overstated the guarantee (low).** Reworded.

Not added: a pure unit test of the quiet/alerting split. It needs a
`CoreUser`, and the e2e delete steps cover it.

## Gate 9c — gate 9b fixes (`59cd278..1b72cef`): PASS

Fixed (both medium; each reopened stale text on screen):
- **A full queue dropped the refresh that removes deleted text.**
  `NoticeQueue` no longer has a cap. It keeps each chat's latest alert
  (a show stays a show when rewritten) and its dismissal, so it is
  bounded by the chats. Covered in
  `the_queue_keeps_each_chats_latest_notices_in_order`.
- **A D-Bus failure lost the ids of notifications still on screen.** A
  failed show keeps the chat's id. A refresh that can't be shown (failed,
  or no longer allowed) closes the old one. Close reconnects, and an id is
  forgotten only once the server closed it or doesn't know it. Covered in
  `stale_text_goes_even_after_the_desktop_failed`, which also covers a
  refresh under `off`. The fake screen records whether a rewrite was quiet.

Deferred:
- **Reactions keep an old alert alive (low).** Reactions are gated only by
  `notified_until`, which desktop doesn't advance, so a quiet rebuild can
  fall back to an old reaction. Upstream desktop has the same limit. The
  fix belongs with milestone 9 (reactions): advance `notified_until` once
  an alert is shown, without losing the fallback for deletes.

## Gate 9d — gate 9c fixes (`1b72cef..ee64c10`): PASS

Fixed:
- **A refresh brought back a notification the user had closed (medium).**
  A rewrite that gets a new id means the old one was gone, so the new one
  is closed at once. Covered in
  `a_rewrite_never_brings_back_what_the_user_closed`.
- **A failed close waited for another dismissal (low).** Pending closes are
  retried before every notice (asserted in
  `stale_text_goes_even_after_the_desktop_failed`). CLAUDE.md says exactly
  that.
- **Dismissing all against a hung service could stall for minutes (low).**
  Closing stops at the first failure; the rest are retried with the next
  notice.
- **Every D-Bus error reply counted as "gone" (low).** Kept, and the
  assumption is written down: the spec defines no other failure for
  `CloseNotification`.

Deferred:
- **Retrying on a timer.** Closes are retried only when a notice arrives.
  Listening to `NotificationClosed` and retrying on a timer belong with
  milestone 12 (notification actions), which subscribes to the server's
  signals anyway.

## Gate 9e — gate 9d fixes (`ee64c10..96e99a5`): PASS

Fixed:
- **Ids outlived the notification server that issued them (medium).**
  After a daemon restart the new instance hands the same ids to other
  apps, so a retried close or a `replaces_id` could hit their
  notifications. The notifier now tracks the service's unique bus name and
  forgets every id when it changes (an id from a server it just activated
  is kept). Covered in `ids_from_a_restarted_server_are_never_reused`.
- **A rewrite that got a new id left the old one unreferenced (low).** The
  old id is closed as well; an id that is already gone closes as success.
- **One dismissal against a hung service stalled twice (low).** After a
  failed close, the rest of that notice doesn't wait on the desktop again.

Deferred:
- **"A new id means the user closed it" depends on the server (low).**
  Servers that reuse an unknown `replaces_id` (dunst does) bring a closed
  notification back with new text on a quiet refresh. The real fix is
  tracking `NotificationClosed`, with milestone 12.

## Gate 10 — milestone 8 and gate 9e fixes (`96e99a5..11ba89c`): BLOCK

Fixed:
- **The delete dialog understated delete (high).** `delete_and_erase_chat`
  ends an active group for every member and erases it on all the user's
  devices. The dialog (now naming the group) says so; an e2e step pins it:
  Bob's copy ends when Ada deletes.
- **`:kick` ran past the single-flight and account checks (medium).** The
  removal is built inside `contact_op`'s start closure.
- **A copied name shielded a member from removal (medium).** Members show
  a `#tag` (SHA-256 of the user id, lengthened until unique in the group,
  since clients choose their ids) and `:kick #tag` takes it; a tag matching
  several is refused. `a_copied_name_never_shields_a_member_from_removal`.
- **Ended groups accepted rename, invite and kick (low).** Refused as
  "You're no longer in this group"; nobody is removable in an ended group.
- **Unbounded member list, reloaded on any user's change (low).** At most
  100 rows plus a count; reloads only for the chat or its members.
- Tests added: removal resolution, `parse_title` bounds, confirming the
  dialog runs the operation and returns focus (which found that
  gpui-component's OK path lost focus; fixed), self chat listed as no group.

Deferred:
- **Inviting a contact whose name was copied (medium, part of the above).**
  Contacts have nothing the user can tell apart yet; safety codes
  (milestone 14) give them that. Until then such an invite is refused as
  ambiguous, never guessed.
- **Old `replaces_id` after a failed owner lookup (low).** A notify that
  reconnects to a restarted daemon right after the lookup failed can still
  replace another app's notification. With milestone 12's
  `NotificationClosed` tracking.

## Gate 10b — gate 10 fixes (`11ba89c..03ce28e`): BLOCK

Fixed:
- **Members sharing a uuid collapsed into one (high).** Model ids dropped
  the domain, so `:kick #tag` on a member who registered another's uuid on
  their own domain removed the other. Each row now keeps its core id
  alongside (same order), and its model id is the hash of the whole id.
  `members_sharing_a_uuid_stay_apart`; tag growth in
  `tags_grow_until_unique`.
- **`load_chat`'s doc moved onto the test helper (low).** Moved back.
- **`:kick` completion offered only names (low).** Alike names complete as
  `Name #tag`, which removal accepts.
- **`:kick` without members said nothing, or "No member matches" while
  loading (low).** Refused as "That needs an account" / "Still loading
  the members".
