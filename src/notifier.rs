//! Shows alerts as desktop notifications. On Linux this talks to
//! `org.freedesktop.Notifications` over the session bus, as upstream's
//! direct fallback does. Upstream prefers the notification portal, but
//! that needs a registered app ID and desktop file, which an unpackaged
//! build doesn't have.
//!
//! Each chat has at most one notification on screen: a new alert replaces
//! it, a refresh rewrites it quietly, and a dismissal closes it. In
//! `private` mode the chats share one notification, which says nothing
//! about any of them; it closes once none of them has activity left.
//!
//! The server tells what the user did: clicking a notification opens its
//! chat, and dismissing one advances its chats' `notified_until` to what it
//! showed, as upstream, so what was dismissed isn't alerted again.

use std::collections::HashMap;
use std::sync::Arc;

use aircoreclient::ChatId;
use chrono::{DateTime, Utc};
use tokio::sync::watch;

use crate::session::alerts::{Alert, Attention, Dismiss, Notice, NoticeQueue};

/// Where notifications appear.
pub trait Desktop {
    /// Shows `alert`, replacing notification `replaces` (0 for none);
    /// `quiet` for a rewrite that shouldn't draw attention. Returns the new
    /// notification's id, or `None` if it couldn't be shown.
    fn notify(
        &mut self,
        replaces: u32,
        alert: &Alert,
        quiet: bool,
    ) -> impl Future<Output = Option<u32>> + Send;
    /// Whether notification `id` is gone (closed now, or already).
    fn close(&mut self, id: u32) -> impl Future<Output = bool> + Send;
    /// Which server instance serves notifications now, `None` if that
    /// can't be told (the desktop is unreachable).
    fn owner(&mut self) -> impl Future<Output = Option<Owner>> + Send;
    /// The next thing the user did with a notification; pending while
    /// nothing can be heard. Cancel-safe: it races the notices.
    fn reply(&mut self) -> impl Future<Output = Reply> + Send;
}

/// A notification server instance (its unique bus name); `None` while none
/// runs. Ids mean something only to the instance that gave them out: after
/// a restart, the new one hands the same ids to other apps.
pub type Owner = Option<String>;

/// What a server says the user did with notification `id`. Servers may
/// tell every app about every notification, so only the instance that
/// gave out our ids (`from`) is listened to, and only about them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub from: String,
    pub id: u32,
    pub what: Replied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replied {
    /// Its default action: the user clicked it.
    Clicked,
    /// The user closed it.
    Dismissed,
    /// It timed out, and is gone.
    Expired,
}

/// What the session is asked to do about a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Show the chat, raising the window.
    Open(ChatId),
    /// Advance the chat's `notified_until` to `until`.
    Dismissed { chat: ChatId, until: DateTime<Utc> },
}

/// The platform's notifications.
pub fn system() -> impl Desktop + Send {
    #[cfg(target_os = "linux")]
    return linux::Dbus::default();
    #[cfg(not(target_os = "linux"))]
    return Unsupported;
}

/// Runs until aborted (the session drops it). Notices are handled in the
/// order they were sent. The attention is checked again at show time: the
/// user may have opened the chat, or asked to be shown less, meanwhile.
pub async fn run(
    notices: Arc<NoticeQueue>,
    attention: watch::Receiver<Attention>,
    desktop: impl Desktop + Send,
    mut outcome: impl FnMut(Outcome) + Send,
) {
    let mut notifier = Notifier::new(desktop);
    // Kept across passes: replies about other apps' notifications mustn't
    // keep putting it off.
    let mut retry_at = None;
    loop {
        retry_at = match retry_at {
            _ if notifier.closing.is_empty() => None,
            None => Some(tokio::time::Instant::now() + RETRY_CLOSE),
            due => due,
        };
        tokio::select! {
            notice = notices.next() => {
                let attention = *attention.borrow();
                notifier.handle(notice, &attention).await;
            }
            reply = notifier.desktop.reply() => {
                notifier.replied(reply).into_iter().for_each(&mut outcome);
            }
            // Stale text mustn't wait for the next alert to go.
            () = sleep_until(retry_at) => {
                retry_at = None;
                notifier.retry().await;
            }
        }
    }
}

async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// How long a failed close waits before it is tried again.
const RETRY_CLOSE: std::time::Duration = std::time::Duration::from_secs(30);

/// A chat's alert on screen.
#[derive(Debug, Clone, Copy)]
struct Shown {
    /// The desktop's id of the notification showing it; chats sharing
    /// one have the same.
    id: u32,
    until: Option<DateTime<Utc>>,
    /// When it was shown, to open the newest of a shared one's chats.
    seq: u64,
}

struct Notifier<D> {
    desktop: D,
    shown: HashMap<ChatId, Shown>,
    /// The notification chats share in `private` mode, if one is up.
    shared: Option<u32>,
    seq: u64,
    /// Notifications taken off our books whose close hasn't gone through
    /// yet; retried, in order, before each notice.
    closing: Vec<u32>,
    /// The server instance the ids above belong to.
    owner: Owner,
    /// A close failed while handling this notice: don't wait on the
    /// desktop again for the rest of it.
    stalled: bool,
    /// Which server runs couldn't be told for this notice: the ids above
    /// may be another app's by now, so none is replaced or closed.
    unsure: bool,
    /// The notification last clicked. Servers close it right after, as if
    /// dismissed; that close marks nothing: a click opens one chat, and
    /// the others of a shared one still have activity to alert.
    clicked: Option<u32>,
}

impl<D: Desktop + Send> Notifier<D> {
    fn new(desktop: D) -> Self {
        Self {
            desktop,
            shown: HashMap::new(),
            shared: None,
            seq: 0,
            closing: Vec::new(),
            owner: None,
            stalled: false,
            unsure: false,
            clicked: None,
        }
    }

    async fn handle(&mut self, notice: Notice, attention: &Attention) {
        self.retry().await;
        let shares = attention.content == crate::settings::Notifications::Private;
        match notice {
            Notice::Show(alert) => {
                if let Some(alert) = attention.shown(alert) {
                    self.show(alert, false, shares).await;
                }
            }
            Notice::Refresh(alert) => {
                let Some(replaces) = self.shown.get(&alert.chat).map(|shown| shown.id) else {
                    return;
                };
                let chat = alert.chat;
                let alert = attention.shown(alert);
                if let Some(alert) = &alert
                    && self.shared == Some(replaces)
                {
                    // The shared notification says nothing about the chat:
                    // only what dismissing it would mark changes.
                    if let Some(shown) = self.shown.get_mut(&chat) {
                        shown.until = alert.until;
                    }
                    return;
                }
                // Full text left up under `private` would say too much; and
                // while the server is unknown, a rewrite would be shown
                // anew, drawing attention again.
                let rewritten = match alert.filter(|_| !shares && !self.unsure) {
                    Some(alert) => self.show(alert, true, false).await,
                    None => None,
                };
                // Kept only if rewritten in place. A new id means the old
                // notification was gone (closed by the user, or expired),
                // and a rewrite mustn't bring it back; no id means what is
                // on screen is stale and couldn't be rewritten.
                if rewritten != Some(replaces) {
                    self.dismiss(Dismiss::Chat(chat)).await;
                }
            }
            Notice::Dismiss(dismiss) => self.dismiss(dismiss).await,
        }
    }

    /// Starts on a notice: learns which server runs, and closes what is
    /// waiting to be closed.
    async fn retry(&mut self) {
        self.stalled = false;
        self.follow_owner().await;
        self.close_pending().await;
    }

    /// Returns the notification's id. On failure, whatever the chat had on
    /// screen is still there and still tracked.
    async fn show(&mut self, alert: Alert, quiet: bool, shared: bool) -> Option<u32> {
        let own = self
            .shown
            .get(&alert.chat)
            .map(|shown| shown.id)
            .filter(|id| Some(*id) != self.shared);
        let replaces = if self.unsure {
            // Shown anew; the old one is closed once the server is known.
            None
        } else if shared {
            self.shared
        } else {
            own
        };
        let id = self
            .desktop
            .notify(replaces.unwrap_or(0), &alert, quiet)
            .await?;
        if self.owner.is_none() {
            // That call started a server (D-Bus activation): the id is
            // its. Unknown if unreachable; then the next notice forgets it.
            self.owner = self.desktop.owner().await.flatten();
        }
        self.seq += 1;
        if self.clicked == Some(id) {
            // Shown again since: a close from now on is the user's.
            self.clicked = None;
        }
        let previous = self.shown.insert(
            alert.chat,
            Shown {
                id,
                until: alert.until,
                seq: self.seq,
            },
        );
        if shared
            && let Some(old) = self.shared.replace(id)
            && old != id
        {
            if self.unsure {
                // Moved, not gone: the old one is closed once it can be.
                self.shown
                    .values_mut()
                    .filter(|shown| shown.id == old)
                    .for_each(|shown| shown.id = id);
            } else {
                // The shared notification was gone (closed by the user),
                // and what it showed with it.
                self.shown.retain(|_, shown| shown.id != old);
            }
            self.release(old);
        }
        if let Some(previous) = previous
            && previous.id != id
        {
            self.release(previous.id);
        }
        Some(id)
    }

    /// Closes notification `id` once no chat's alert is in it any more.
    fn release(&mut self, id: u32) {
        if self.shown.values().any(|shown| shown.id == id) {
            return;
        }
        if self.shared == Some(id) {
            self.shared = None;
        }
        if !self.closing.contains(&id) {
            self.closing.push(id);
        }
    }

    /// Forgets every id once another server instance took over: closing or
    /// replacing one would hit another app's notification. The window
    /// between this check and the calls after it is left open.
    async fn follow_owner(&mut self) {
        let Some(owner) = self.desktop.owner().await else {
            self.unsure = true;
            return;
        };
        self.unsure = false;
        if owner != self.owner {
            self.shown.clear();
            self.shared = None;
            self.closing.clear();
            self.owner = owner;
        }
    }

    async fn dismiss(&mut self, dismiss: Dismiss) {
        match dismiss {
            Dismiss::Chat(chat) => {
                if let Some(shown) = self.shown.remove(&chat) {
                    self.release(shown.id);
                }
            }
            Dismiss::All => {
                let ids: Vec<u32> = self.shown.drain().map(|(_, shown)| shown.id).collect();
                ids.into_iter().for_each(|id| self.release(id));
            }
        }
        self.close_pending().await;
    }

    /// Closes what is waiting to be closed, stopping at the first failure:
    /// a desktop that just failed is likely to fail again right away.
    async fn close_pending(&mut self) {
        while !self.stalled
            && !self.unsure
            && let Some(&id) = self.closing.first()
        {
            if !self.desktop.close(id).await {
                self.stalled = true;
                return;
            }
            self.closing.remove(0);
        }
    }

    /// What the user did with one of our notifications asks of the session.
    /// A notification that is gone takes its chats' alerts with it, so a
    /// rewrite won't bring it back.
    fn replied(&mut self, reply: Reply) -> Vec<Outcome> {
        if self.owner.as_ref() != Some(&reply.from) {
            return Vec::new();
        }
        let chats: Vec<(ChatId, Shown)> = self
            .shown
            .iter()
            .filter(|(_, shown)| shown.id == reply.id)
            .map(|(chat, shown)| (*chat, *shown))
            .collect();
        match reply.what {
            Replied::Clicked => {
                if !chats.is_empty() {
                    self.clicked = Some(reply.id);
                }
                chats
                    .iter()
                    .max_by_key(|(_, shown)| shown.seq)
                    .map(|(chat, _)| Outcome::Open(*chat))
                    .into_iter()
                    .collect()
            }
            Replied::Dismissed | Replied::Expired => {
                self.shown.retain(|_, shown| shown.id != reply.id);
                if self.shared == Some(reply.id) {
                    self.shared = None;
                }
                self.closing.retain(|&id| id != reply.id);
                let clicked = self.clicked.take_if(|id| *id == reply.id).is_some();
                if reply.what == Replied::Expired || clicked {
                    return Vec::new();
                }
                chats
                    .into_iter()
                    .filter_map(|(chat, shown)| {
                        Some(Outcome::Dismissed {
                            chat,
                            until: shown.until?,
                        })
                    })
                    .collect()
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
struct Unsupported;

#[cfg(not(target_os = "linux"))]
impl Desktop for Unsupported {
    async fn notify(&mut self, _: u32, _: &Alert, _: bool) -> Option<u32> {
        None
    }
    async fn close(&mut self, _: u32) -> bool {
        true
    }
    async fn owner(&mut self) -> Option<Owner> {
        None
    }
    async fn reply(&mut self) -> Reply {
        std::future::pending().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Notifications;

    /// What a desktop shows, by id. Calls fail while `down`.
    #[derive(Default)]
    struct Screen {
        next_id: u32,
        visible: HashMap<u32, (Alert, bool)>,
        down: bool,
        /// The server instance running, if any.
        server: Option<u32>,
        started: u32,
        /// Which server runs can't be told, though notifying works.
        lookup_down: bool,
    }

    impl Screen {
        /// The server stops with its notifications; ids start over.
        fn restart(&mut self) {
            self.visible.clear();
            self.next_id = 0;
            self.server = None;
        }
    }

    impl Desktop for Screen {
        async fn notify(&mut self, replaces: u32, alert: &Alert, quiet: bool) -> Option<u32> {
            if self.down {
                return None;
            }
            if self.server.is_none() {
                self.started += 1;
                self.server = Some(self.started);
            }
            let id = if self.visible.contains_key(&replaces) {
                replaces
            } else {
                self.next_id += 1;
                self.next_id
            };
            self.visible.insert(id, (alert.clone(), quiet));
            Some(id)
        }
        async fn close(&mut self, id: u32) -> bool {
            if self.down {
                return false;
            }
            self.visible.remove(&id);
            true
        }
        async fn owner(&mut self) -> Option<Owner> {
            (!self.down && !self.lookup_down).then(|| self.server.map(|n| n.to_string()))
        }
        async fn reply(&mut self) -> Reply {
            std::future::pending().await
        }
    }

    fn alert(chat: u128, body: &str) -> Alert {
        Alert {
            chat: ChatId::new(uuid::Uuid::from_u128(chat)),
            title: "Grace".into(),
            body: body.into(),
            until: None,
        }
    }

    /// An alert standing for the chat's activity up to `at` seconds.
    fn alert_until(chat: u128, body: &str, at: i64) -> Alert {
        Alert {
            until: DateTime::from_timestamp(at, 0),
            ..alert(chat, body)
        }
    }

    impl Alert {
        /// As `private` shows it.
        fn private(self, at: i64) -> Self {
            Self {
                title: "Air".into(),
                until: DateTime::from_timestamp(at, 0),
                ..self
            }
        }
    }

    /// The user does `what` to the notification showing `body`, as the
    /// running server reports it.
    fn user_does(notifier: &mut Notifier<Screen>, body: &str, what: Replied) -> Vec<Outcome> {
        let id = notifier
            .desktop
            .visible
            .iter()
            .find(|(_, (alert, _))| alert.body == body)
            .map(|(id, _)| *id)
            .expect("on screen");
        if what != Replied::Clicked {
            notifier.desktop.visible.remove(&id);
        }
        let from = notifier.desktop.server.unwrap().to_string();
        notifier.replied(Reply { from, id, what })
    }

    fn bodies(notifier: &Notifier<Screen>) -> Vec<String> {
        let mut bodies: Vec<_> = notifier
            .desktop
            .visible
            .values()
            .map(|(alert, _)| alert.body.clone())
            .collect();
        bodies.sort();
        bodies
    }

    #[tokio::test]
    async fn what_is_shown_follows_the_latest_notices_and_setting() {
        let mut notifier = Notifier::new(Screen::default());
        let full = Attention::default();
        for notice in [
            Notice::Show(alert(1, "secret")),
            Notice::Show(alert(2, "hello")),
            // Edited: rewritten in place, one notification per chat.
            Notice::Refresh(alert(2, "hello!")),
            // A rewrite never brings up a chat with nothing on screen.
            Notice::Refresh(alert(3, "old")),
            Notice::Dismiss(Dismiss::Chat(alert(1, "").chat)),
        ] {
            notifier.handle(notice, &full).await;
        }
        assert_eq!(bodies(&notifier), ["hello!"]);
        // The rewrite didn't draw attention again.
        let (_, quiet) = notifier.desktop.visible.values().next().unwrap();
        assert!(quiet);

        // An alert built under "full" but shown after the switch to
        // "private" reveals nothing.
        let private = Attention {
            content: Notifications::Private,
            ..full
        };
        notifier
            .handle(Notice::Dismiss(Dismiss::All), &private)
            .await;
        notifier
            .handle(Notice::Show(alert(4, "the secret")), &private)
            .await;
        assert_eq!(bodies(&notifier), ["New activity"]);
    }

    #[tokio::test]
    async fn stale_text_goes_even_after_the_desktop_failed() {
        let mut notifier = Notifier::new(Screen::default());
        let full = Attention::default();
        notifier
            .handle(Notice::Show(alert(1, "secret")), &full)
            .await;

        // The rewrite after a delete fails, and so does closing: the stale
        // notification stays tracked, and the next dismissal closes it.
        notifier.desktop.down = true;
        notifier
            .handle(Notice::Refresh(alert(1, "older")), &full)
            .await;
        assert_eq!(bodies(&notifier), ["secret"]);
        notifier.desktop.down = false;
        notifier
            .handle(Notice::Dismiss(Dismiss::Chat(alert(1, "").chat)), &full)
            .await;
        assert!(bodies(&notifier).is_empty());

        // A rewrite the user may no longer see (setting off) closes it.
        notifier.handle(Notice::Show(alert(2, "hi")), &full).await;
        let off = Attention {
            content: Notifications::Off,
            ..full
        };
        notifier
            .handle(Notice::Refresh(alert(2, "hi!")), &off)
            .await;
        assert!(bodies(&notifier).is_empty());

        // A failed close is retried with the next notice.
        notifier
            .handle(Notice::Show(alert(3, "secret")), &full)
            .await;
        notifier.desktop.down = true;
        let third = alert(3, "").chat;
        notifier
            .handle(Notice::Dismiss(Dismiss::Chat(third)), &full)
            .await;
        notifier.desktop.down = false;
        notifier.handle(Notice::Show(alert(4, "hi")), &full).await;
        assert_eq!(bodies(&notifier), ["hi"]);
    }

    #[tokio::test]
    async fn a_rewrite_never_brings_back_what_the_user_closed() {
        let mut notifier = Notifier::new(Screen::default());
        let full = Attention::default();
        notifier
            .handle(Notice::Show(alert(1, "older")), &full)
            .await;
        // The user closes it by hand.
        notifier.desktop.visible.clear();
        notifier
            .handle(Notice::Refresh(alert(1, "older")), &full)
            .await;
        assert!(bodies(&notifier).is_empty());
    }

    #[tokio::test]
    async fn a_click_opens_and_a_dismissal_marks_only_what_was_shown() {
        let mut notifier = Notifier::new(Screen::default());
        let full = Attention::default();
        let (first, second) = (alert(1, "").chat, alert(2, "").chat);
        notifier
            .handle(Notice::Show(alert_until(1, "hi", 10)), &full)
            .await;
        // Edited: what dismissing it marks follows what it shows.
        notifier
            .handle(Notice::Refresh(alert_until(1, "hi!", 20)), &full)
            .await;
        notifier
            .handle(Notice::Show(alert_until(2, "yo", 30)), &full)
            .await;

        assert_eq!(
            user_does(&mut notifier, "yo", Replied::Clicked),
            [Outcome::Open(second)]
        );
        // Another server instance's word about the same id counts for
        // nothing.
        let id = notifier.shown[&first].id;
        let stranger = Reply {
            from: "elsewhere".into(),
            id,
            what: Replied::Dismissed,
        };
        assert!(notifier.replied(stranger).is_empty());
        assert_eq!(
            user_does(&mut notifier, "hi!", Replied::Dismissed),
            [Outcome::Dismissed {
                chat: first,
                until: DateTime::from_timestamp(20, 0).unwrap(),
            }]
        );
        // Gone, so a rewrite doesn't bring it back.
        notifier
            .handle(Notice::Refresh(alert_until(1, "hi?", 40)), &full)
            .await;
        assert_eq!(bodies(&notifier), ["yo"]);

        // One that timed out is forgotten without marking anything.
        assert!(user_does(&mut notifier, "yo", Replied::Expired).is_empty());
        notifier
            .handle(Notice::Refresh(alert_until(2, "yo!", 50)), &full)
            .await;
        assert!(bodies(&notifier).is_empty());
    }

    #[tokio::test]
    async fn private_alerts_share_one_notification_until_no_chat_needs_it() {
        let mut notifier = Notifier::new(Screen::default());
        let private = Attention {
            content: Notifications::Private,
            ..Attention::default()
        };
        let (first, second) = (alert(1, "").chat, alert(2, "").chat);
        notifier
            .handle(Notice::Show(alert_until(1, "secret", 10)), &private)
            .await;
        notifier
            .handle(Notice::Show(alert_until(2, "other", 20)), &private)
            .await;
        assert_eq!(bodies(&notifier), ["New activity"]);

        // Opening one chat leaves it up for the other.
        notifier
            .handle(Notice::Dismiss(Dismiss::Chat(first)), &private)
            .await;
        assert_eq!(bodies(&notifier), ["New activity"]);
        notifier
            .handle(Notice::Dismiss(Dismiss::Chat(second)), &private)
            .await;
        assert!(bodies(&notifier).is_empty());

        notifier
            .handle(Notice::Show(alert_until(1, "secret", 10)), &private)
            .await;
        notifier
            .handle(Notice::Show(alert_until(2, "other", 20)), &private)
            .await;
        // An edit under `private` changes nothing on screen, only what a
        // dismissal marks.
        notifier
            .handle(Notice::Refresh(alert_until(1, "secret!", 15)), &private)
            .await;
        let on_screen: Vec<_> = notifier.desktop.visible.values().cloned().collect();
        assert_eq!(on_screen, [(alert(2, "New activity").private(20), false)]);
        // A dismissal marks every chat in it.
        let mut marked = user_does(&mut notifier, "New activity", Replied::Dismissed);
        marked.sort_by_key(|outcome| match outcome {
            Outcome::Dismissed { until, .. } => *until,
            Outcome::Open(_) => DateTime::UNIX_EPOCH,
        });
        assert_eq!(
            marked,
            [
                Outcome::Dismissed {
                    chat: first,
                    until: DateTime::from_timestamp(15, 0).unwrap(),
                },
                Outcome::Dismissed {
                    chat: second,
                    until: DateTime::from_timestamp(20, 0).unwrap(),
                },
            ]
        );

        // A click opens the newest chat, and the close the server sends
        // after it marks nothing: the other chat's activity is unseen.
        notifier
            .handle(Notice::Show(alert_until(1, "secret", 10)), &private)
            .await;
        notifier
            .handle(Notice::Show(alert_until(2, "other", 20)), &private)
            .await;
        assert_eq!(
            user_does(&mut notifier, "New activity", Replied::Clicked),
            [Outcome::Open(second)]
        );
        assert!(user_does(&mut notifier, "New activity", Replied::Dismissed).is_empty());

        // Closed by the user without our hearing of it: the next alert
        // gets a new shared notification, and only its chat is in it.
        notifier
            .handle(Notice::Show(alert_until(1, "secret", 10)), &private)
            .await;
        notifier.desktop.visible.clear();
        notifier
            .handle(Notice::Show(alert_until(3, "new", 30)), &private)
            .await;
        assert_eq!(bodies(&notifier), ["New activity"]);
        assert_eq!(notifier.shown.len(), 1);

        // Back to full text: new alerts get their own notification, and
        // the shared one stays for its chat.
        let full = Attention::default();
        notifier
            .handle(Notice::Show(alert_until(4, "hello", 40)), &full)
            .await;
        assert_eq!(bodies(&notifier), ["New activity", "hello"]);
    }

    /// A screen the test can look at while `run` owns it, and replies the
    /// test sends.
    struct Watched {
        screen: Arc<std::sync::Mutex<Screen>>,
        replies: tokio::sync::mpsc::UnboundedReceiver<Reply>,
    }

    impl Watched {
        /// The screen's calls never wait, so they finish under the lock.
        fn now<T>(&self, call: impl FnOnce(&mut Screen) -> T) -> T {
            call(&mut self.screen.lock().unwrap())
        }
    }

    fn at_once<T>(future: impl Future<Output = T>) -> T {
        use futures_util::FutureExt;
        future.now_or_never().expect("the screen never waits")
    }

    impl Desktop for Watched {
        async fn notify(&mut self, replaces: u32, alert: &Alert, quiet: bool) -> Option<u32> {
            self.now(|screen| at_once(screen.notify(replaces, alert, quiet)))
        }
        async fn close(&mut self, id: u32) -> bool {
            self.now(|screen| at_once(screen.close(id)))
        }
        async fn owner(&mut self) -> Option<Owner> {
            self.now(|screen| at_once(screen.owner()))
        }
        async fn reply(&mut self) -> Reply {
            match self.replies.recv().await {
                Some(reply) => reply,
                None => std::future::pending().await,
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn replies_reach_the_session_and_failed_closes_retry_meanwhile() {
        use std::time::Duration;
        let screen = Arc::new(std::sync::Mutex::new(Screen::default()));
        let (replies_tx, replies) = tokio::sync::mpsc::unbounded_channel();
        let notices = Arc::new(NoticeQueue::default());
        let (_attention, attention) = watch::channel(Attention::default());
        let (outcomes_tx, mut outcomes) = tokio::sync::mpsc::unbounded_channel();
        let desktop = Watched {
            screen: screen.clone(),
            replies,
        };
        let running = tokio::spawn(run(notices.clone(), attention, desktop, move |outcome| {
            _ = outcomes_tx.send(outcome)
        }));
        let (first, second) = (alert(1, "").chat, alert(2, "").chat);
        notices.push(Notice::Show(alert(1, "hi")));
        notices.push(Notice::Show(alert(2, "stale")));
        tokio::time::sleep(Duration::from_secs(1)).await;

        let (from, hi) = {
            let screen = screen.lock().unwrap();
            let hi = screen.visible.iter().find(|(_, (a, _))| a.chat == first);
            (screen.server.unwrap().to_string(), *hi.unwrap().0)
        };
        let reply = |id, what| Reply {
            from: from.clone(),
            id,
            what,
        };
        replies_tx.send(reply(hi, Replied::Clicked)).unwrap();
        assert_eq!(outcomes.recv().await, Some(Outcome::Open(first)));

        // Closing the stale one fails; replies about other apps'
        // notifications keep coming, and the retry still comes due.
        screen.lock().unwrap().down = true;
        notices.push(Notice::Dismiss(Dismiss::Chat(second)));
        tokio::time::sleep(Duration::from_secs(1)).await;
        screen.lock().unwrap().down = false;
        for _ in 0..4 {
            tokio::time::sleep(Duration::from_secs(10)).await;
            replies_tx.send(reply(999, Replied::Expired)).unwrap();
        }
        let left: Vec<_> = {
            let screen = screen.lock().unwrap();
            screen
                .visible
                .values()
                .map(|(a, _)| a.body.clone())
                .collect()
        };
        assert_eq!(left, ["hi"]);
        running.abort();
    }

    #[tokio::test]
    async fn an_id_is_never_replaced_while_the_server_is_unknown() {
        let mut notifier = Notifier::new(Screen::default());
        let full = Attention::default();
        notifier.handle(Notice::Show(alert(1, "old")), &full).await;
        // The server restarted, and gave our old id to another app; which
        // server runs can't be told right now.
        notifier.desktop.restart();
        notifier.desktop.server = Some(9);
        notifier.desktop.next_id = 1;
        notifier
            .desktop
            .visible
            .insert(1, (alert(7, "theirs"), false));
        notifier.desktop.lookup_down = true;
        // Shown anew; the old id would be closed now, were the server
        // known.
        notifier.handle(Notice::Show(alert(1, "new")), &full).await;
        assert_eq!(bodies(&notifier), ["new", "theirs"]);
        // Once it can be told, the old id is forgotten, not closed.
        notifier.desktop.lookup_down = false;
        notifier.retry().await;
        assert_eq!(bodies(&notifier), ["new", "theirs"]);
    }

    #[tokio::test]
    async fn ids_from_a_restarted_server_are_never_reused() {
        let mut notifier = Notifier::new(Screen::default());
        let full = Attention::default();
        notifier.handle(Notice::Show(alert(1, "old")), &full).await;
        notifier.desktop.restart();
        // The new server gives chat 2's notification the old one's id.
        notifier.handle(Notice::Show(alert(2, "new")), &full).await;
        let first = alert(1, "").chat;
        notifier
            .handle(Notice::Refresh(alert(1, "older")), &full)
            .await;
        notifier
            .handle(Notice::Dismiss(Dismiss::Chat(first)), &full)
            .await;
        assert_eq!(bodies(&notifier), ["new"]);
        // What the new server showed is still ours to close.
        let second = alert(2, "").chat;
        notifier
            .handle(Notice::Dismiss(Dismiss::Chat(second)), &full)
            .await;
        assert!(bodies(&notifier).is_empty());
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;

    use std::time::Duration;

    use futures_util::StreamExt;
    use zbus::fdo::DBusProxy;
    use zbus::names::{BusName, WellKnownName};
    use zbus::proxy::SignalStream;
    use zbus::zvariant::Value;
    use zbus::{Connection, Message, Proxy};

    use tokio::sync::mpsc;
    use tracing::{info, warn};

    use crate::session::alerts::Alert;

    /// How long a missing notification service is left alone.
    const RETRY: Duration = Duration::from_secs(60);

    /// The session bus, connected on the first alert and again after a
    /// failure; a missing service is warned about once and retried slowly.
    #[derive(Default)]
    pub(super) struct Dbus {
        bus: Option<Bus>,
        failed_at: Option<tokio::time::Instant>,
    }

    impl Dbus {
        async fn bus(&mut self) -> Option<&Bus> {
            if self.bus.is_none() {
                if self.failed_at.is_some_and(|at| at.elapsed() < RETRY) {
                    return None;
                }
                match Bus::connect().await {
                    Ok(bus) => {
                        if self.failed_at.take().is_some() {
                            info!("desktop notifications available again");
                        }
                        self.bus = Some(bus);
                    }
                    Err(err) => {
                        let now = tokio::time::Instant::now();
                        if self.failed_at.replace(now).is_none() {
                            warn!("desktop notifications unavailable: {err}");
                        }
                    }
                }
            }
            self.bus.as_ref()
        }
    }

    impl super::Desktop for Dbus {
        async fn notify(&mut self, replaces: u32, alert: &Alert, quiet: bool) -> Option<u32> {
            let result = self.bus().await?.notify(replaces, alert, quiet).await;
            result
                .inspect_err(|err| {
                    warn!("failed to show a desktop notification: {err}");
                    self.bus = None;
                })
                .ok()
        }

        async fn owner(&mut self) -> Option<super::Owner> {
            let result = self.bus().await?.owner().await;
            match result {
                Ok(owner) => Some(Some(owner)),
                Err(zbus::fdo::Error::NameHasNoOwner(_)) => Some(None),
                Err(err) => {
                    warn!("failed to look up the notification service: {err}");
                    self.bus = None;
                    None
                }
            }
        }

        async fn reply(&mut self) -> super::Reply {
            loop {
                // Connected by the first alert: before it, nothing of ours
                // is on screen to hear about.
                let Some(bus) = self.bus.as_mut() else {
                    return std::future::pending().await;
                };
                match bus.replies.recv().await {
                    Some(reply) => return reply,
                    // The connection is gone; the next alert reconnects.
                    None => self.bus = None,
                }
            }
        }

        async fn close(&mut self, id: u32) -> bool {
            let Some(bus) = self.bus().await else {
                return false;
            };
            match bus.close(id).await {
                // An error reply (the spec's reply to an unknown id, or the
                // service having gone away with its notifications) means
                // it isn't on screen. Assumed for every error name: the
                // spec defines no other failure for this call.
                Ok(()) | Err(zbus::Error::MethodError(..)) => true,
                Err(err) => {
                    warn!("failed to close a desktop notification: {err}");
                    self.bus = None;
                    false
                }
            }
        }
    }

    /// Matches the window's app id, so the desktop can group them.
    const APP_ID: &str = "air-desktop";

    /// A hung notification service mustn't stall the notifier.
    const CALL_TIMEOUT: Duration = Duration::from_secs(5);

    async fn bounded<T>(call: impl Future<Output = zbus::Result<T>>) -> zbus::Result<T> {
        tokio::time::timeout(CALL_TIMEOUT, call)
            .await
            .map_err(|_| zbus::Error::Failure("timed out".into()))?
    }

    const SERVICE: &str = "org.freedesktop.Notifications";

    /// Replies waiting for the notifier. The user acts on one notification
    /// at a time; beyond this, replies are dropped (a click is lost).
    const REPLIES: usize = 32;

    /// Reads the service's signals as they come, whatever the notifier is
    /// doing: unread ones would fill zbus's queue, and then its reader
    /// waits, holding back the replies to our calls. The service signals
    /// about every app's notifications, and about our own closes.
    async fn pump(mut signals: SignalStream<'static>, replies: mpsc::Sender<super::Reply>) {
        while let Some(message) = signals.next().await {
            if let Some(reply) = reply_from(&message)
                && let Err(mpsc::error::TrySendError::Closed(_)) = replies.try_send(reply)
            {
                return;
            }
        }
    }

    /// Stops the pump with its connection.
    struct Pump(tokio::task::JoinHandle<()>);

    impl Drop for Pump {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    struct Bus {
        proxy: Proxy<'static>,
        dbus: DBusProxy<'static>,
        /// What the user did with notifications, as the pump hears it.
        replies: mpsc::Receiver<super::Reply>,
        _pump: Pump,
        /// The server renders a subset of HTML in bodies.
        markup: bool,
    }

    impl Bus {
        async fn connect() -> zbus::Result<Self> {
            bounded(Self::connect_unbounded()).await
        }

        async fn connect_unbounded() -> zbus::Result<Self> {
            let connection = Connection::session().await?;
            let dbus = DBusProxy::new(&connection).await?;
            let proxy = Proxy::new(
                &connection,
                SERVICE,
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
            )
            .await?;
            // Subscribed before anything is shown, so no reply is missed.
            let signals = proxy.receive_all_signals().await?;
            let (replies_tx, replies) = mpsc::channel(REPLIES);
            let pump = Pump(tokio::spawn(pump(signals, replies_tx)));
            let capabilities: Vec<String> = proxy.call("GetCapabilities", &()).await?;
            let markup = capabilities.iter().any(|c| c == "body-markup");
            Ok(Self {
                proxy,
                dbus,
                replies,
                _pump: pump,
                markup,
            })
        }

        async fn owner(&self) -> zbus::fdo::Result<String> {
            let name = BusName::from(WellKnownName::from_static_str_unchecked(SERVICE));
            tokio::time::timeout(CALL_TIMEOUT, self.dbus.get_name_owner(name))
                .await
                .map_err(|_| zbus::fdo::Error::TimedOut("timed out".into()))?
                .map(|owner| owner.to_string())
        }

        async fn notify(&self, replaces: u32, alert: &Alert, quiet: bool) -> zbus::Result<u32> {
            // Peer text must not become markup: a link or image tag would
            // be live in some notification servers.
            let body = if self.markup {
                escape_markup(&alert.body)
            } else {
                alert.body.clone()
            };
            let args = notify_args(replaces, &alert.title, &body, quiet);
            bounded(self.proxy.call("Notify", &args)).await
        }

        async fn close(&self, id: u32) -> zbus::Result<()> {
            bounded(self.proxy.call("CloseNotification", &(id,))).await
        }
    }

    const DEFAULT_ACTION: &str = "default";

    /// `Notify`'s arguments, `susssasa{sv}i` as the spec has it.
    type NotifyArgs<'a> = (
        &'a str,
        u32,
        &'a str,
        &'a str,
        &'a str,
        Vec<&'a str>,
        HashMap<&'a str, Value<'a>>,
        i32,
    );

    fn notify_args<'a>(
        replaces: u32,
        title: &'a str,
        body: &'a str,
        quiet: bool,
    ) -> NotifyArgs<'a> {
        let mut hints: HashMap<&str, Value> = HashMap::new();
        // GNOME 46+ matches notifications to apps by pid.
        hints.insert("sender-pid", Value::from(std::process::id()));
        hints.insert("desktop-entry", Value::from(APP_ID));
        hints.insert("category", Value::from("im.received"));
        if quiet {
            hints.insert("suppress-sound", Value::from(true));
            hints.insert("urgency", Value::from(0u8));
        }
        (
            "Air",
            replaces,
            APP_ID,
            title,
            body,
            // Clicking the notification; servers without actions ignore it.
            // A Vec: a fixed-size array would be sent as a struct.
            vec![DEFAULT_ACTION, "Open"],
            hints,
            -1,
        )
    }

    /// The spec's close reasons.
    const EXPIRED: u32 = 1;
    const DISMISSED: u32 = 2;

    /// A signal as a reply, if it is one we act on. Closes by a
    /// `CloseNotification` call (reason 3) are ours, already accounted for;
    /// and a stale one could name an id the server has since handed out
    /// again.
    fn reply_from(message: &Message) -> Option<super::Reply> {
        use super::Replied;
        let header = message.header();
        let from = header.sender()?.to_string();
        let (id, what) = match header.member()?.as_str() {
            "ActionInvoked" => {
                let (id, action): (u32, String) = message.body().deserialize().ok()?;
                (id, (action == DEFAULT_ACTION).then_some(Replied::Clicked)?)
            }
            "NotificationClosed" => {
                let (id, reason): (u32, u32) = message.body().deserialize().ok()?;
                let what = match reason {
                    EXPIRED => Replied::Expired,
                    DISMISSED => Replied::Dismissed,
                    _ => return None,
                };
                (id, what)
            }
            _ => return None,
        };
        Some(super::Reply { from, id, what })
    }

    /// Escapes the characters the notification markup subset gives meaning.
    pub(super) fn escape_markup(text: &str) -> String {
        let mut escaped = String::with_capacity(text.len());
        for c in text.chars() {
            match c {
                '&' => escaped.push_str("&amp;"),
                '<' => escaped.push_str("&lt;"),
                '>' => escaped.push_str("&gt;"),
                '"' => escaped.push_str("&quot;"),
                '\'' => escaped.push_str("&#39;"),
                c => escaped.push(c),
            }
        }
        escaped
    }

    #[cfg(test)]
    mod tests {
        use super::super::{Replied, Reply};
        use zbus::Message;

        fn signal(
            member: &str,
            body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
        ) -> Message {
            Message::signal(
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
                member,
            )
            .unwrap()
            .sender(":1.5")
            .unwrap()
            .build(body)
            .unwrap()
        }

        /// Shows and closes a notification on this machine's desktop.
        #[tokio::test]
        #[ignore = "needs a desktop session's notification server"]
        async fn desktop_notify_and_close() {
            use crate::notifier::Desktop as _;
            let mut desktop = super::Dbus::default();
            let alert = crate::session::alerts::Alert {
                chat: aircoreclient::ChatId::new(uuid::Uuid::from_u128(1)),
                title: "air-desktop test".into(),
                body: "closes by itself".into(),
                until: None,
            };
            let id = desktop.notify(0, &alert, true).await.expect("shown");
            assert!(desktop.owner().await.flatten().is_some());
            assert!(desktop.close(id).await);
        }

        #[test]
        fn notify_is_called_as_the_spec_declares_it() {
            let args = super::notify_args(3, "Grace", "hi", true);
            let call = Message::method_call("/org/freedesktop/Notifications", "Notify")
                .unwrap()
                .build(&args)
                .unwrap();
            assert_eq!(call.body().signature().to_string(), "(susssasa{sv}i)");
        }

        #[test]
        fn only_clicks_and_the_users_closes_are_replies() {
            let reply = |what| {
                Some(Reply {
                    from: ":1.5".into(),
                    id: 7,
                    what,
                })
            };
            let read = |message: Message| super::reply_from(&message);
            assert_eq!(
                read(signal("ActionInvoked", &(7u32, "default"))),
                reply(Replied::Clicked)
            );
            assert_eq!(read(signal("ActionInvoked", &(7u32, "inline-reply"))), None);
            assert_eq!(
                read(signal("NotificationClosed", &(7u32, 2u32))),
                reply(Replied::Dismissed)
            );
            assert_eq!(
                read(signal("NotificationClosed", &(7u32, 1u32))),
                reply(Replied::Expired)
            );
            // Closed by a call: ours, or stale for an id handed out again.
            assert_eq!(read(signal("NotificationClosed", &(7u32, 3u32))), None);
        }

        #[test]
        fn peer_text_never_becomes_markup() {
            assert_eq!(
                super::escape_markup(r#"<img src="http://x/t.png"> & <a href='x'>"#),
                "&lt;img src=&quot;http://x/t.png&quot;&gt; &amp; &lt;a href=&#39;x&#39;&gt;"
            );
        }
    }
}
