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

Milestones 15 and on cover upstream features the core already supports
and the desktop app doesn't show yet (compared with upstream's app at the
pinned rev). Safety and privacy features come first.

15. **Contact safety** (done). Block and unblock a contact, report spam, delete
    a one-to-one chat, and retract an outgoing contact request. Each is
    confirmed and has a `:` command.
16. **Message deletion and mute** (done). Delete a message for yourself or for
    everyone from the message cursor (confirmed). Mute and unmute a chat
    (`:mute [duration]`; alerts already skip muted chats). Add message
    details.
17. **Account management** (done). Delete the account, including upstream's
    abort handling. Settings rows for read receipts and for deleting
    expired contact requests (both synced by the core). Invitation codes.
18. **Profile pictures** (done). Set your own and show peers' pictures, decoded
    under the same limits as attachments.
19. **Polish** (done). Emoji picker and autocomplete, a full-size image viewer,
    retrying a failed upload, and adding a contact from a group.
20. **Account seam tests** (done). The real core against Air's in-process test
    server, in session, workspace and app-root tests, covering the
    deferred teardown, routing and late-result tests from the review log.

Milestones 21 and on close the remaining gaps with upstream's app at the
pinned rev. Upstream's share target and developer account switching stay
out; desktop notifications stay Linux-only, as is the package.

21. **Messaging gaps** (done). Jump to a replied-to message, animated images,
    large emoji-only messages, and a contact request's expiry countdown.
22. **App and support screens** (done). About and licenses, help, logs, debug info,
    the update-required screen and reminder, and a username suggestion.
23. **Release readiness**. A PKGBUILD from tagged releases for the AUR, a
    `.desktop` file and icon, a data-directory lock, and the tests still
    open in the review log.
24. **Translations and interface scale**. Move the UI's strings to a
    translation system, reuse upstream's where the meaning is the same, fall
    back to English otherwise; an interface scale setting.
