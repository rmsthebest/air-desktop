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
