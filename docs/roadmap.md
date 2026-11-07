# Roadmap

Milestones 1–7 follow the original plan (scaffold through polish). The
"later" list and the review gates' deferred items are ordered here as
milestones. Each one ends with a review gate (see CLAUDE.md).

8. **Groups and members** (done). Create a group, invite contacts, remove members
   (as the room policy allows), rename, leave and delete. Members are shown
   in the details pane. Keyboard and `:` commands for each.
9. **Reactions** (done). Show reactions under messages, and add or retract one
   from the keyboard (quick picker on the message cursor).
10. **Attachments and images** (done). Show and download received files
    and images (size caps, no automatic fetches beyond what the core does).
    Send files from a picker or by drag and drop. Downloads are automatic,
    as in upstream's app (only from Air's own storage, nothing third-party);
    retrying a failed upload is left for later.
11. **Settings screen** (done). A view for theme, appearance, keymap,
    notifications, display name and usernames, next to the commands.
12. **Notification actions** (done). Clicking a notification opens its chat and
    raises the window (D-Bus `ActionInvoked`, Wayland activation token).
    In `private` mode, one shared notification instead of one per chat.
    A notification the user dismisses advances the chat's `notified_until`
    (as upstream), so old reactions stop coming back in rewritten alerts.
    Under Wayland the window may only be marked as wanting attention:
    gpui has no way to activate it with the server's token.
13. **Multi-device linking** (done). Link and unlink devices. With a second
    device of the same user, cover sibling notification suppression in
    the e2e tests (deferred since gate 9).
14. **Safety codes** (done). Show and compare a contact's safety code.
