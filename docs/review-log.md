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

## Gate 10c — gate 10b fixes (`03ce28e..570ab43`): BLOCK

Fixed:
- **A name ending in `#<tag>` redirected a removal (high).** A member named
  `Mallory #<grace's tag>` got that completion, and the trailing tag won:
  Grace was removed, unconfirmed. Now a query is a tag only when all of it
  is `#<hex>`; completion shows `Name #tag` but inserts the name only if it
  resolves back to that member (else the tag); `:kick` resolves first and
  confirms "Remove <name> (#tag)?", then removes exactly that core id.
  `every_completion_finds_its_member_whatever_the_names`.
- **Quadratic alike-name count (low).** Counted with a map. The session
  still loads every member (the pane caps rows); accepted: groups that
  large aren't expected yet, and upstream loads them all too.
- **A failed load looked loaded (low).** Recorded; `:kick` refuses with
  "Couldn't load the members".
- **`tags_grow_until_unique` couldn't fail (test).** Now tests the length
  rule on chosen hashes sharing ten digits.

Deferred:
- A test of the "Still loading" refusal needs a session; covered by
  reading, with the e2e for the loaded path.

## Gate 10d — gate 10c fixes (`570ab43..24acd35`): PASS

Fixed:
- **`unreachable!` kept alive by an early return (low).** Member
  completion is an arm of the match.

Accepted:
- **A failed reload leaves the pane's list in place (low).** Removal
  refuses while it is stale; the only way a reload fails is the load task
  itself failing, which is logged. A "couldn't refresh" note can come with
  the settings/details work (milestone 11).
- Test suggestions (case/space-alike names; comparing tags instead of
  positions in the completion test) taken as nice-to-haves: identical
  normalized names already exercise the alike path.

Moved (milestone 9): the gate 9c deferral "reactions keep an old alert
alive" goes to milestone 12. Upstream advances `notified_until` when the
user dismisses a notification, which needs the server's
`NotificationClosed` signal that milestone subscribes to.

## Gate 11 — milestone 9 (`d0ac5d6..baf147a`): PASS

Fixed:
- **One peer could inflate a count with invisible variants (medium).**
  Reactors are merged per label as a set: each counts once. Covered in
  `reactions_are_bounded_and_never_hide_the_users_own`.
- **The picker outlived its message (low).** A splice that drops the
  cursor closes it.
- **Quick toggles raced (low).** The view decides add or retract from what
  it shows (`set_reaction`); a repeated add is a no-op in the core.
- **Alerts showed a reaction's whole text (low).** They use
  `reaction_label`, as the app does.
- **Quadratic merge (low).** Labels are indexed in a map.
- Tests: the variation-selector label of ❤️, and the standard keymap's
  picker keys.

Deferred:
- A behaviour test for the splice case needs a live message window (the
  mock one only appends); the rule is one line next to the cursor shift.

## Gate 11b — gate 11 fixes (`baf147a..0785445`): PASS

Fixed:
- **Retracting missed an own variant (low).** Retracting deletes every
  own reaction that labels the same, as the view shows them as one (e2e:
  a `👍` + zero-width space variant goes with `👍`).
- **An invisible picker kept its keys (low).** The picker closes whenever
  the cursor's message can't take reactions (deleted in place, too).
- **"Don't race" overstated (low).** The doc says what two quick presses
  do.
- Tests: the add/retract decision is `model::gave`, tested; every quick
  reaction labels as itself.

## Gate 11c — gate 11b fixes (`0785445..09e9604`): PASS

Accepted (documented on `set_reaction`): a failed retract can be partial
(pressing again finishes it); a retract on a stale view does nothing.
The picker-closes-on-delete behaviour test stays deferred (needs a live
message window).

## Gate 12 — attachments, milestone 10 (`845e79a..cde7df9`): BLOCK

Fixed:
- **Sent files weren't queued (high).** After the upload the message is
  enqueued with the outbound service, and a failed send marked failed,
  as upstream; before, it went out only on the next wake (up to 60 s)
  and a failed one showed "sending" forever.
- **Peer-controlled preview memory (medium).** A decoded preview is kept
  at most at twice its drawn box (720×540, 1.5 MiB); at most 64 previews
  per window, past that images show as file cards.
- **Dropped files sent unasked (medium).** A drop asks first, naming the
  chat and the files; files are sent one after another, in order.
- **Stuck downloads (medium).** `s` on a waiting file downloads it now
  (an arrival the loop missed, say); a downloaded image without a
  preview gets one when shown.
- **Concurrent downloads of one file (low).** One download per
  attachment at a time (`IN_FLIGHT`), shared by the loop and `s`.
- **Misplaced doc comments (low).** Moved back.
- Tests: the attachment-part parser (`declared`), and the decode keeping
  at most the drawn size.

Deferred:
- A `#[gpui::test]` for releasing preview textures: the test platform's
  atlas doesn't expose what it holds.

## Gate 12b — gate 12 fixes (`cde7df9..69a351b`): PASS

Fixed:
- **A preview-less file read again on every reload (medium).** The
  on-demand preview is tried once: for a file declared an image, the
  core's thumbnail is generated whatever the bytes, and its stored
  failure stops later tries.
- **The picture budget counted pictures on their way out (medium).**
  Messages refused a preview for want of room are reloaded once room is
  made (`Key::undrawn`, `fill_pictures`).
- **A drop's confirmation could send to another chat (low).** The chat
  named in the dialog is the one the files go to.
- **`s` gave the wrong reason (low).** Already downloading, done
  meanwhile (reported, so the view catches up), and failed are told
  apart.
- Found by the new claim test: taking an attachment already being
  downloaded deadlocked (`then_some` built and dropped a claim under the
  lock). Fixed.
- Tests: a drop asks first, sends nothing when cancelled, and runs once
  confirmed; one claim per attachment.

Accepted: the on-demand preview runs on the message worker (once per
file, as above), not off it.

## Gate 12c — gate 12b fixes (`69a351b..286d1f8`): PASS

Fixed in `6c68979` (all low):
- Only declared images wait for a preview slot, and freed slots go
  newest first.
- The file dialog sends to the chat open when it was opened; if another
  chat was opened meanwhile, nothing is sent and the user is told.
- `s` on a file the core can't download says so.
- The claim test fails, rather than hangs, if the deadlock returns.

Deferred:
- A test for the picture budget refill (needs a live message window, or
  more of the worker made pure), and an e2e check that a declared image
  with undecodable bytes is read only once.
- A test that a confirmed drop goes to the chat the dialog named needs a
  session seam in the workspace tests.

## Gate 12d — gate 12c fixes (`286d1f8..6c68979`): PASS

Fixed in `3525a3f`: an unreachable branch removed (the core refuses an
image it can't decode at upload, and stores its thumbnail with it).

Accepted: a WebP not declared an image that misses a preview slot stays
a file card (fails closed).

Deferred: tests for `retry` per status and for a file dialog answered
after the chat changed (need a core-backed seam, or a file-dialog seam,
in workspace tests).

## Gate 12e — gate 12d fixes (`6c68979..3525a3f`): PASS

Fixed in `35c3afb` (low):
- A file dialog answered after the chat closed told the user twice, the
  second time wrongly: back to `sendable_chat`'s one refusal.
- `retry` downloads an attachment stored as downloading while holding
  its claim (a quit whose reset didn't happen).

## Gate 12f — gate 12e fixes (`3525a3f..35c3afb`): PASS

Fixed (low):
- `s` on a file shown as downloading goes to the session too, so the
  stuck case above is reachable from the view; a real download in
  this process still answers "Already downloading" (its claim).
- A file dialog answered after another chat was opened always says
  nothing was sent, whatever that chat is (a request one included).

Accepted: the claim is per process. Two instances on one data
directory could fetch the same file twice (as the startup reset already
assumes one instance); a data-directory lock is for later.

Deferred: a core-backed test of `retry` on a stale downloading
attachment versus a claimed one.

## Gate 12g — gate 12f fixes (`35c3afb..785d78c`): PASS

Fixed (low): a file dialog answered with no chat open says the chat was
closed, not that another was opened.

Accepted: without an account, `s` on a downloading file says files need
one (mock mode only).

Still deferred: the file-dialog seam test (same chat, another chat, no
chat) and the core-backed `retry` test.

## Gate 13 — settings screen, milestone 11 (`7f64842..2f3040b`): PASS

Fixed:
- **The chat behind the settings counted as looked at (medium).** While
  the settings are shown, attention reports no open chat (alerts show,
  and other devices aren't told to stay quiet), and the hidden
  conversation marks nothing as seen; it measures what is in view anew
  once drawn again.
- **Left on a username offered its removal (low).** Only the forward key
  (enter, right) acts on text and list rows.
- **Tab couldn't reach the settings (low)** from the chat list.
- **No word after removing a username (low)**, from the screen or
  `:remove-username`.
- Tests: stepping back on a username does nothing; a theme from the
  settings changes only its half and keeps the appearance.

Deferred (need an account seam in workspace tests): attention while the
settings are open, and a username removal waiting for its confirm.

## Gate 13b — gate 13 fixes (`2f3040b..73044cc`): PASS

Fixed (low):
- Hiding the conversation also forgets the last drawn range, so the
  first frame after the settings close reports what is in view.
- Opening a chat from the settings reports attention once, for the new
  chat, not the old one first.
- `shown_changed`'s doc back on it.
- Tests: the theme test goes through the settings event (catches a
  rewire to `:theme`'s appearance-changing path); stepping back does
  nothing on every text row.

Deferred: a `Conversation` test that hide then unhide still marks seen
(needs drawn frames in a test window); the attention-order test (needs
an account seam).

## Gate 14 — notification actions, milestone 12 (`115dd15..1818cd1`): BLOCK

Milestone 12 also closed earlier deferrals: retrying closes on a timer
(gate 9d), dunst reusing a closed notification's id (gate 9e; closed
notifications are now forgotten on `NotificationClosed`), and an old
`replaces_id` after a failed owner lookup (gate 10; nothing is replaced
or closed while the server is unknown).

Fixed:
- **Every notification failed on real desktops (high).** The actions
  were a fixed-size array, which zvariant sends as a struct
  (`(susss(ss)a{sv}i)`), so servers refused `Notify`. Now a `Vec`. The
  arguments come from `notify_args`, whose signature a unit test pins,
  and an ignored test (`desktop_notify_and_close`) shows and closes one on
  the machine's real server. It failed on the old code against Plasma.
- **Unread signals could stall the connection (low).** A pump task reads
  the service's signals as they come and keeps only replies we act on
  (bounded, extra ones dropped), so zbus's queue never fills and holds
  back method replies.
- **The close retry could be put off forever (low).** Its deadline is
  kept across loop passes. Covered in
  `replies_reach_the_session_and_failed_closes_retry_meanwhile` (paused
  time; it fails with a restarting timer).
- **A click counted as a dismissal (low).** Servers close a clicked
  notification as dismissed; that close now marks nothing, so a shared
  notification's other chats stay unnotified.
- **A click could report the previous chat as looked at (low).** Attention
  is reported directly only when the clicked chat was already open behind
  the settings; otherwise `Opened` reports it.
- **Clicking an alert for a chat not yet listed did nothing (low).** It
  opens once listed.
- Tests: an edit under `private` changes nothing on screen, but changes
  what a dismissal marks; a shared notification closed without our
  hearing of it doesn't bring its chats back; signal parsing.

Accepted: on Wayland the window may only be marked as wanting attention,
since gpui can't activate with the server's token. A notification shown
while the server was unknown is forgotten if the server turns out to have
restarted.

Deferred (needs an account seam): a workspace test that opening from an
alert with the settings open reports attention once, for the clicked chat.
The e2e step pins that an alert's `until` covers what it shows; the path
from notifier outcome to `set_chat_notified_until` has no test.

## Gate 14b — gate 14 fixes (`1818cd1..7abe130`): PASS

Fixed (low):
- Signals are no longer dropped when the notifier is busy (the pump's
  channel is unbounded; it fills only while a call runs, which times out).
  A lost close of ours would have kept a reused id tracked (gate 9e).
- The close after a click spares only what the click saw: what was shown
  in the notification after it is still marked by that close (or by a
  later real dismissal, on servers that keep clicked notifications).
- An alert for a chat not yet listed, clicked with the settings open,
  reports what the settings hid and focuses the chat list until the chat
  opens. Test: `an_alert_for_a_chat_not_yet_listed_opens_it_once_listed`.
- `desktop_notify_and_close` checks the close reply itself.

## Gate 14c — gate 14b fixes (`7abe130..eae030f`): PASS

Fixed (low):
- Only a close within 2 s of a click is taken as the server's: a server
  that keeps clicked notifications leaves the later dismissal to the
  user, which marks everything shown. Tests: a click then the close marks
  nothing; `a_clicked_notification_kept_on_screen_is_the_users_to_dismiss`.
- Commands that open a chat not yet listed (`:contact`, accept, `:group`)
  focus the chat list while waiting, as alerts do; both focus the chat
  once it opens (`open_then_focus`). The alert test checks the final focus.
- Correction to gate 14b's reasoning: our own closes (reason 3) never
  reach the channel. What a dropped reply could lose is a click, or a
  user's close.

Still deferred (account seam): attention reported for what the settings
hid while an alert's chat isn't listed, then for the chat once it opens.

## Gate 14d — gate 14c fixes (`eae030f..01522b5`): PASS

Fixed:
- **A chat listed late took focus from wherever the user went (medium).**
  The awaited chat takes focus only if the chat list still has it.
  `a_chat_listed_late_never_takes_focus_from_where_the_user_went` (fails
  without the check); `opening_another_chat_ends_the_wait`.
- **The click window ran from handling, not arrival (low).** Replies are
  stamped when the pump reads them, so a close queued behind slow calls
  still counts as the click's.
  `the_close_right_after_a_click_marks_nothing_even_if_handled_late`.
- The long `private` test no longer repeats the plain click-then-close
  case; that has its own test now.

Accepted: an `Opened` already queued for another chat ends a wait set
after it (focus stays on the chat list). Deferred: the command path for a
contact-request chat focusing its messages (needs a request chat in the
mock data listed late).

## Gate 14e — gate 14d fixes (`01522b5..c807d95`): PASS

Fixed (low):
- The workspace's wait could outlive the chat list's (picking the chat
  already open cancels the list's wait without an `Opened`), so browsing
  onto the awaited chat later moved focus. The list now emits
  `AwaitedOpened` only when a wait it still holds opens the chat, and only
  that event focuses. `a_cancelled_wait_never_moves_focus_later` (fails
  on the old handling) replaces `opening_another_chat_ends_the_wait`,
  which no `Workspace` regression could fail.
- A `private` test comment no longer claims a check made elsewhere.

## Gate 15 — milestone 13 (`3f4bf40..3e15766`): PASS

Fixed:
- **Escape couldn't leave link mode at onboarding (medium).** Entering it
  left focus on the hidden name field, outside the card's key context.
  The server field takes focus now.
  `escape_leaves_a_link_that_is_under_way` (fails without the fix).
- **Opening the settings over the link pane lost focus (low).** It goes to
  the pane. Covered in `a_link_goes_ahead_only_once_confirmed_…`, which
  also checks a confirmed link that fails after its pane closed.
- **Teardown could be aborted between deleting the database and its
  record (low).** It runs detached.
- **After teardown, onboarding even with another account left (low).** The
  app loads again; onboarding (with the notice) only if nothing loads.
- **Every chat change reloaded the device list without a self chat
  (low).** Only before the first load. `the_list_reloads_for_…`.
- Tests: the QR test (it tested the crate) is gone; the link-failure
  test no longer checks constant messages for secrets.

Accepted: `:device-name #beef desk` reads `#beef` as a tag (a leading
`#hex` word is one, as documented); refused, never misapplied.

Deferred:
- **Alerts from an unlinked account stay on screen (low).** The notifier
  is dropped without closing them; closing every tracked id before the
  session goes needs a teardown path in the notifier.
- **No test that teardown releases the session (account seam).**

## Gate 15b — gate 15 fixes (`3e15766..67ac71f`): PASS

Fixed:
- **A failing teardown restarted the unlinked account in a loop
  (medium).** Gate 15's "load again" applied to failures too: the record
  was still there, so the same account loaded, was found unlinked and
  failed again. Only a successful deletion loads again; a failed one goes
  to onboarding (the next start retries), as before gate 15.
- **A new device's list could stay without its self chat (medium).** Gate
  15 stopped reloading on chat changes once loaded without a self chat,
  but a new device joins its self group after its first load. As
  upstream, a chat change then looks the self chat up (one query) and
  reloads once it exists. `the_list_reloads_for_…` asserts the probe.
- **The notice was lost when another account loaded (low).** It shows as
  a toast in that account's workspace.
- **Teardown comment overstated (low).** Quitting stops the runtime, so
  it can still cut the deletion short; the next start skips the bare
  record (logged). Not worth blocking quit for.
- The link-failure test checks again that an internal error's detail
  isn't shown (the one arm that could regress to `{err:#}`).

Still deferred (account seam): a failed teardown landing on onboarding,
and the notice surviving another account loading.

## Gate 15c — gate 15b fixes (`67ac71f..edc28b6`): PASS

Fixed:
- **A failed teardown was routed on the wrong guess (medium).** The core
  deletes the record even when removing a file failed, so "the next start
  retries" was false: the database (keys, history) stayed with nothing
  pointing at it, and onboarding showed even with another account left.
  `tear_down` now says whether the record survived: gone, the app loads
  again (no loop possible); kept, onboarding as before. Record-less
  client databases (`<id>.db`, `-shm`, `-wal`) are removed whenever an
  account is loaded: `only_client_databases_without_a_record_are_removed`.
  The e2e test checks a linked account has a finished record (so the
  sweep never takes one) and that a clean teardown reports `Deleted`.
- **A chat change during a running self-chat lookup was dropped (low).**
  A new change replaces the lookup, so the latest one always runs, as
  upstream's per-notification check. Correction to gate 15b's entry: the
  test covers the classification only, not the lookup and reload.

Still deferred (account seam): routing a failed teardown by its record,
and a self-chat lookup outrun by the Welcome, in a `#[gpui::test]`.

## Gate 15d — gate 15c fixes (`edc28b6..433483d`): PASS

Fixed:
- **The sweep could delete a link under way in another instance
  (medium).** The core writes a new account's record last, so a database
  without one may still be in the making (nothing stops a second
  instance on the same data dir). Gate 15c's blind sweep is gone: a
  teardown whose record went but whose files stayed marks its id
  (`<id>.deleted`), and only marked ids' files are removed when an
  account loads (in `spawn_blocking`).
  `only_marked_databases_without_a_record_are_removed` keeps an unmarked
  record-less database.
- **Busy chats could starve the self-chat lookup (low).** Gate 15c
  replaced a running lookup on every change. Now the lookup runs to its
  end and one more follows if a chat changed meanwhile.
  `a_change_during_a_lookup_gets_one_more_lookup`.

Still deferred (account seam): `AppRoot::tear_down`'s routing.

## Gate 15e — gate 15d fixes (`433483d..b6ba09e`): PASS

Fixed (low):
- **A quit or unreadable records after the core dropped the record left
  the files unmarked for good.** The mark is written before deleting and
  dropped on success. The e2e test checks no mark stays after a clean
  teardown.
- A file that can't be removed keeps its mark (tested with a directory
  in the way), so a later load retries.

Accepted: such a file logs an error on every load until it goes; the
user isn't told (nothing they can act on in the app).
Deferred (account seam): the subscriber's "look again, don't replace"
branch; only `after_probe` is tested.

## Gate 15f — gate 15e fixes (`b6ba09e..e798019`): PASS

Fixed (low): `mark_deleted`'s comment described the old order (it is
written before every teardown now); the mark and its directory are
synced, so a power loss can't keep the record's removal but lose the
mark. A failed mark is logged as possibly needing manual cleanup.

Accepted: a mark beside a record that was kept does nothing until the
record goes (its meaning); that account loading again is the deferred
teardown-routing item.

## Gate 15g — gate 15f fixes (`e798019..c694e33`): PASS

Fixed (low): marking and unmarking run in `spawn_blocking` (they fsync);
the directory sync is Unix-only (Windows can't open a directory so).

## Gate 15h — gate 15g fixes (`c694e33..a1ca068`): PASS

Accepted (low): a failed unmark task (panic, runtime shutdown) is not
logged; the mark it leaves is dropped by the next load's sweep, which
finds nothing to remove. Windows doesn't sync the mark's directory, so a
power loss there may lose a mark (the app targets Linux).

## Gate 16 — milestone 14 (`5691cd7..f69b847`): PASS

Fixed (low):
- A name after `:safety-code` in a one-to-one chat was ignored, and the
  contact's code was shown under it. It is refused now. The checks made
  before the session is asked are the pure `safety_target`;
  `a_safety_code_is_asked_for_only_from_someone_to_compare_with` (fails
  without the fix) also covers the self-chat refusal, which the mock
  data couldn't reach.
- A code that resolved after the user opened another chat was still
  shown. It is dropped now, and a new request replaces a pending one.
- A missing peer credential (a local lookup) said to check the
  connection. It says the credential isn't on this device yet.
- `Members::find` and `resolve` duplicated their load checks; they share
  `index`. `a_safety_code_is_never_the_users_own` (fails without the
  own-entry filter) pins that `find` skips the user, whatever the policy.
- The e2e `assert_ne!` compared the user's code with itself against a
  pair: it couldn't fail for a plausible regression, so it is dropped.
- CLAUDE.md now says one-to-one codes need an accepted request.

## Gate 16b — gate 16 fixes (`f69b847..9265ad7`): PASS

Fixed (low): every failed peer lookup claimed the credential was
missing, though the core also fails on a database error, and the own
lookup still blamed the connection. Both lookups now say the code
couldn't be computed on this device (try again if the contact is new)
and log the cause; the core's error can't tell a missing credential
apart. The new workspace test split the test module's imports.

Accepted: a code that resolves while another dialog is open opens over
it (it is still the open chat's code). Deferred (account seam): a
`#[gpui::test]` that a late code for another chat is dropped and a new
request replaces a pending one; mock data stops before the lookup.

## Gate 16c — gate 16b fixes (`9265ad7..4e4f7d8`): PASS

Fixed (low): a peer credential that hasn't arrived yet (expected) was
logged as an error; the peer lookup logs a warning, the own lookup
(which should never fail) an error.

## Gate 17 — milestone 15 (`5617b19..be28733`): PASS

Fixed:
- **Two rule tables (medium).** The view's pre-check copied the
  session's and already disagreed (unblock on a blocked request; report
  or retract of a request to a username asked first, then refused).
  There is one now, `model::ContactOp::check`, on `OneToOne` facts that
  `ChatSummary` carries; the view only words the dialog.
  `each_contact_operation_applies_only_where_it_may` replaces both tests.
- **A blocked request's sender couldn't be unblocked (medium).** As
  reported, it assumed the chat stays, blocked; the core in fact erases
  it when blocking. Any blocked one-to-one chat that is left now shows
  the Unblock footer, and the dialog for blocking a request says it
  takes away the only place to unblock them. The e2e test covers the
  path: the request is erased and the sender's next request never shows.
  Deferred: a blocked-contacts list (the core's is `pub(crate)`;
  upstream has none either).
- `e`/`r` in a chat without a composer focused a composer that isn't
  drawn; they do nothing now. A chat that stops taking messages while
  its composer has focus (blocked from another device too) moves focus
  to the messages, in `Conversation::update_chat`. Next unread skips
  blocked chats. Deleting a contact's chat says their chat ends too.
  Tests: the blocked-chat gpui test (each fails without its fix), the
  `next_unread` test, `contact_dialogs_say_what_really_happens`.

## Gate 17b — gate 17 fixes (`be28733..38cd8ad`): PASS

Fixed (low):
- The delete dialog's wording went by side alone: a contact's chat they
  ended said "their chat with you ends", and a request to a blocked
  peer (maybe open on their side) said it had ended. It goes by side
  and stage now; such a request is deleted, not retracted (`check`
  says so instead of "ended already").
- A contact's chat the peer deleted (inactive) still had a composer.
  It has a footer with Delete now; `composes` allows only open stages,
  so focus moves off a composer that goes this way too.
  `only_a_live_chat_is_written_to` and the blocked-chat gpui test fail
  without it.
- The e2e check that a blocked sender's next request never shows slept
  5 s, so a slow listener passed it wrongly. A third account asks on the
  same username after him; once Ada has that request, hers has been
  past his. The test takes three codes now.
- Tests: the chats test pins only the core-to-facts mapping (adding an
  outgoing request to a blocked peer and a contact's ended chat); the
  request mapping is the model's. The dialog test no longer repeats a
  rule.

Kept on purpose: the Unblock footer on a blocked request's chat. The
core erases such a chat when blocking, so it shows only if one is left,
but it follows `check` and costs nothing.

## Gate 17c — gate 17b fixes (`38cd8ad..ded1b28`): PASS

Fixed (low): the e2e check that a blocked sender's next request never
shows matched only open requests, so one stored blocked or ended would
pass. It now fails on any chat of a request from him, whatever its
status (his earlier one was erased with the block).

## Gate 18 — milestone 16, message deletion, details and mute (`cba7ca2..8b3aeba`): PASS

Fixed:
- (medium) The view and the session decided "this chat takes messages"
  differently: a group the user is out of still composed in the view
  (so shift-d asked, then the session refused) and kept a composer.
  One rule now, `model::takes_messages`, on `one_to_one` facts and
  `group_ended` (`chats::group_ended`, which both sides derive from the
  core's chat). An ended group shows a Delete footer and refuses sends.
  `only_a_live_chat_is_written_to` fails without it.
- (low) `erasable` was computed when a message loaded, from whether the
  request was open, and went stale once it was answered. The view now
  counts a request's system messages as erasable (`may_erase`); the
  session checks the real state.
- (low) The edit history converted every version before keeping 20:
  only the shown ones are converted now (`EditHistory::new` takes how
  many were skipped).
- (low) Two doc comments sat on the wrong items (`load_chat`,
  `contact_confirmation`).
- (low) Muting left the chat's alert on screen: it is dismissed now.
- Tests: the details step (only "a dialog opened") and the e2e mute
  block (the core's call, not the session's) are dropped; the mute test
  keeps only the expiry the list redraws on.

## Gate 18b — gate 18 fixes (`8b3aeba..efd3625`): PASS

Fixed (low):
- A group in any status but active counted as live (the session used to
  require active). `chats::group_ended` now covers every non-active
  group status; the core gives groups only active or inactive today.
- The ended-group footer and focus move had no view test: the
  blocked-chat gpui test now ends a group under a focused composer
  (focus moves to the messages; typing, `e` and `r` do nothing). It
  fails if `composes` ignores `group_ended`.
- Tests: kept the mute boundary (`is_muted` false at its end); dropped
  an edit-history case no caller can produce.

Accepted (low): muting can lose a race with alert processing that read
the chat as unmuted just before, leaving that one alert up until the
next rewrite or dismissal. Narrow and harmless; not worth re-reading
the chat before every notice.

## Gate 19 — milestone 17, account management (`ad08cae..522bce2`): PASS

Fixed (low):
- Invite codes were shown and marked copied in a cleaned-up form, while
  the core marks the stored code exactly: a code that needed cleaning
  never showed as given out, and a changed code would not work.
  `model::invite_code` now leaves out (logs) any code that isn't plain
  letters and digits, never alters one, so what is shown, copied and
  marked is the stored code.
- Removing the codes given out (one `l` on the row) deleted them all
  unasked: it is confirmed now. Copying on `l` is kept: it only marks a
  code given out, which loses nothing.
- A second account deletion while one ran could report an abort twice:
  it is refused while one runs.
- The synced-setting rows flipped the core's default before the stored
  values loaded: they show only once loaded.
- Tests: the toggle test no longer restates `change` both ways; it
  checks the rows wait for the load (fails if they show defaults).

Deferred to milestone 20 (account seam): a test of the abort-report gate
in `Session::check_deletion_abort` (needs a fake for the two core calls).

## Gate 19b — gate 19 fixes (`522bce2..86f0e6f`): PASS

Fixed (low):
- The settings selection is kept by position, so the synced rows
  appearing on load moved it onto another row (a device, a username)
  that Delete would then act on. The rows now show from the start, `…`
  and not flippable until loaded, so the load moves no row. The test
  checks the row count stays and the unloaded rows change nothing.
- A code from the server that isn't plain letters and digits was a
  generic error, though the token was spent: it is now told
  (`Failure::Shown`, it cost an invite).
- Test: removing given-out codes asks first (Escape removes nothing,
  confirmed it runs); fails if the confirmation is dropped.

Deferred to milestone 20 (account seam): `invitations::load`'s filtering,
and the one-deletion-at-a-time refusal with the abort-report test.

## Gate 19c — gate 19b fixes (`86f0e6f..58881dc`): PASS

Fixed (low):
- Invite and device rows also load late, and moved a selection kept by
  position: on Delete account, a token loading above it put the
  selection on `GetInviteCode`, so enter spent it unasked. The selection
  now follows its row (`reselect`: same username, device tag, code,
  token, or kind of row), clamped where it was if the row is gone.
  Test: a token loading above Delete account keeps the selection there.

Deferred to milestone 20 (account seam): `Invitations::request` turning
an unusual server code into `Failure::Shown`.

## Gate 20 — milestone 18, profile pictures (`f6ca347..dd713ad`): PASS

Fixed:
- (medium) A `Members` entity, made each time a group opens, never
  dropped its pictures' GPU textures: every reopen left them uploaded.
  `Members`, `Chats` and the `Session` (own picture) now drop them on
  release, as `Messages` does.
- `read_source` read a path by its reported size: `/dev/zero` or a pipe
  would read forever or block. It now checks the open file is a regular
  one and reads at most the cap (`read_bounded`, tested with
  `/dev/zero`, a directory and a sparse oversized file).
- The rule keeping a requester's or blocked contact's picture off screen
  was applied inline in `load_chat`, untested there: the bytes shown are
  now chosen by `chats::picture_shown`, which the test exercises.
- `PictureOf` had taken `expand_home`'s doc comment.
- `:remove-picture` and `:remove-group-picture` now ask first, as the
  settings row does (`Workspace::remove_picture`); the test covers the
  typed command too.

Deferred to milestone 20 (account seam): a test that `Members` releases
its textures (needs a session), and one for `pick_picture` refusing when
another chat was opened during the file dialog (needs a test file
dialog). Reopening a group decodes its members' pictures again (the
cache lives with the entity): bounded by the members, accepted.

## Gate 20b — gate 20 fixes (`dd713ad..6f85a5d`): PASS

Fixed (low):
- Opening a named pipe blocks until a writer comes, before the
  regular-file check could run: `:picture ~/x.fifo` hung a blocking
  thread. The file is now opened with `O_NONBLOCK` (regular files ignore
  it); the test makes a FIFO with `mkfifo` and fails on a hang.
- `:remove-group-picture`'s confirmation is tested (on a mock group).

Still deferred to milestone 20: a test that `Members` releases its
textures.

## Gate 20c — gate 20b fixes (`6f85a5d..39d2aec`): PASS

Fixed (low): the non-blocking open, `libc` and the device/pipe test
cases were Linux-only; they are `cfg(unix)` now (a named pipe blocks
`open` on macOS too).

Accepted (low): the test makes its FIFO with the `mkfifo` program, not
`libc::mkfifo` (which needs `unsafe`); coreutils is assumed present.

## Gate 21 — milestone 19, polish (`f5ed341..b4ea001`): PASS

Fixed (low):
- The image viewer could start a large decode per keypress: `v` is now
  single-flight (`VIEWING`, "Still opening an image"), and only an image
  whose thumbnail the core made (or whose original it says fits) opens,
  never a failed one's original.
- `:contact-member` refused only a pending request of the user's; any
  existing contact-request chat with the member now refuses before the
  core.
- Shortcodes expanded on any change ending in `:smile:` (a paste, an
  undo, moving the cursor and editing elsewhere); only a closing colon
  just typed expands now (`emoji::closed_by_typing`). The test that
  checked the `emojis` crate's data became one on that rule.
- The picker built every emoji's item on each render; they're built once.
- Doc comments that had drifted off `position_of`, `show_image` and
  `show_details` are back.
- The refusal tests checked only that no dialog opened; they now check
  the text the user is told (`ui::last_told`, test only), and `+ /`
  reacting is tested in Helix mode too (where `/` elsewhere finds a chat).

Deferred to milestone 20 (account seam): a session test that a failed
thumbnail's original never opens in the viewer, and one that
`retry_upload` holds the `IN_FLIGHT` claim.

Accepted (low): the viewer checks the file's size after taking the
single-flight claim (the load is bounded anyway); an expansion replaces
the composer's whole text (bounded by the input).

## Gate 21b — gate 21 fixes (`b4ea001..71c3280`): PASS

Fixed (medium):
- Gate 21's `:contact-member` check over-corrected: refusing on any
  contact-request chat with the member kept refusing after the request
  expired (the core keeps the chat, deletes its record). It now checks
  what the core does, `try_targeted_message_contact`.
- `ui::last_told` kept everything told, so the Helix pass of the
  reaction test could pass on the default pass's text. It now drains the
  record.

Fixed (low):
- `set_value` emits no Change, so after a restored draft, an edit or a
  send the shortcode check compared with stale text and a typed `:`
  didn't expand. `set_composer_text` (also used by submit now) keeps it
  in step; `shortcodes_expand_only_as_typed` covers that and undo.
- A second `v` while an image opened dropped the first load's view task,
  so nothing showed. The viewer has its own pending slot and refuses
  while it's taken.

Deferred to milestone 20: an e2e or seam test that `:contact-member`
works again after a request through a group was retracted or expired.
Accepted: `the_viewer_opens_one_image_at_a_time` tests only the guard;
the session test is deferred with the account seam.

## Gate 21c — gate 21b fixes (`71c3280..f440395`): PASS

Fixed (medium): the viewer's own pending slot wasn't dropped when the
chat changed (the shared `details` slot had been), so an image still
opening could show over the next chat, and `v` there was refused until
it ended. Opening or leaving a chat drops it now.

Deferred to milestone 20 (account seam): a gpui test that a pending view
is dropped on a chat change (mock chats have no account, so no load
ever starts).
