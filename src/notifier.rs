//! Shows alerts as desktop notifications. On Linux this talks to
//! `org.freedesktop.Notifications` over the session bus, as upstream's
//! direct fallback does. Upstream prefers the notification portal, but
//! that needs a registered app ID and desktop file, which an unpackaged
//! build doesn't have.
//!
//! Each chat has at most one notification on screen: a new alert replaces
//! it, a refresh rewrites it quietly, and a dismissal closes it.

use std::collections::HashMap;
use std::sync::Arc;

use aircoreclient::ChatId;
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
    fn close(&mut self, id: u32) -> impl Future<Output = ()> + Send;
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
) {
    let mut notifier = Notifier {
        desktop,
        shown: HashMap::new(),
    };
    loop {
        let notice = notices.next().await;
        let attention = *attention.borrow();
        notifier.handle(notice, &attention).await;
    }
}

struct Notifier<D> {
    desktop: D,
    /// The desktop's id of each chat's notification.
    shown: HashMap<ChatId, u32>,
}

impl<D: Desktop + Send> Notifier<D> {
    async fn handle(&mut self, notice: Notice, attention: &Attention) {
        match notice {
            Notice::Show(alert) => {
                if let Some(alert) = attention.shown(alert) {
                    self.show(alert, false).await;
                }
            }
            Notice::Refresh(alert) => {
                if !self.shown.contains_key(&alert.chat) {
                    return;
                }
                let chat = alert.chat;
                match attention.shown(alert) {
                    Some(alert) => self.show(alert, true).await,
                    None => self.dismiss(Dismiss::Chat(chat)).await,
                }
            }
            Notice::Dismiss(dismiss) => self.dismiss(dismiss).await,
        }
    }

    async fn show(&mut self, alert: Alert, quiet: bool) {
        let replaces = self.shown.get(&alert.chat).copied().unwrap_or(0);
        match self.desktop.notify(replaces, &alert, quiet).await {
            Some(id) => self.shown.insert(alert.chat, id),
            None => self.shown.remove(&alert.chat),
        };
    }

    async fn dismiss(&mut self, dismiss: Dismiss) {
        let ids: Vec<u32> = match dismiss {
            Dismiss::Chat(chat) => self.shown.remove(&chat).into_iter().collect(),
            Dismiss::All => self.shown.drain().map(|(_, id)| id).collect(),
        };
        for id in ids {
            self.desktop.close(id).await;
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
    async fn close(&mut self, _: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Notifications;

    /// What a desktop shows, by id.
    #[derive(Default)]
    struct Screen {
        next_id: u32,
        visible: HashMap<u32, Alert>,
    }

    impl Desktop for Screen {
        async fn notify(&mut self, replaces: u32, alert: &Alert, _: bool) -> Option<u32> {
            let id = if self.visible.contains_key(&replaces) {
                replaces
            } else {
                self.next_id += 1;
                self.next_id
            };
            self.visible.insert(id, alert.clone());
            Some(id)
        }
        async fn close(&mut self, id: u32) {
            self.visible.remove(&id);
        }
    }

    fn alert(chat: u128, body: &str) -> Alert {
        Alert {
            chat: ChatId::new(uuid::Uuid::from_u128(chat)),
            title: "Grace".into(),
            body: body.into(),
        }
    }

    fn bodies(notifier: &Notifier<Screen>) -> Vec<String> {
        let mut bodies: Vec<_> = notifier
            .desktop
            .visible
            .values()
            .map(|alert| alert.body.clone())
            .collect();
        bodies.sort();
        bodies
    }

    #[tokio::test]
    async fn what_is_shown_follows_the_latest_notices_and_setting() {
        let mut notifier = Notifier {
            desktop: Screen::default(),
            shown: HashMap::new(),
        };
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
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;

    use std::time::Duration;

    use zbus::zvariant::Value;
    use zbus::{Connection, Proxy};

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

        async fn close(&mut self, id: u32) {
            if let Some(bus) = &self.bus {
                // Fails if it is already gone, which is fine.
                _ = bus.close(id).await;
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

    struct Bus {
        proxy: Proxy<'static>,
        /// The server renders a subset of HTML in bodies.
        markup: bool,
    }

    impl Bus {
        async fn connect() -> zbus::Result<Self> {
            bounded(Self::connect_unbounded()).await
        }

        async fn connect_unbounded() -> zbus::Result<Self> {
            let connection = Connection::session().await?;
            let proxy = Proxy::new(
                &connection,
                "org.freedesktop.Notifications",
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
            )
            .await?;
            let capabilities: Vec<String> = proxy.call("GetCapabilities", &()).await?;
            let markup = capabilities.iter().any(|c| c == "body-markup");
            Ok(Self { proxy, markup })
        }

        async fn notify(&self, replaces: u32, alert: &Alert, quiet: bool) -> zbus::Result<u32> {
            // Peer text must not become markup: a link or image tag would
            // be live in some notification servers.
            let body = if self.markup {
                escape_markup(&alert.body)
            } else {
                alert.body.clone()
            };
            let mut hints: HashMap<&str, Value> = HashMap::new();
            // GNOME 46+ matches notifications to apps by pid.
            hints.insert("sender-pid", Value::from(std::process::id()));
            hints.insert("desktop-entry", Value::from(APP_ID));
            hints.insert("category", Value::from("im.received"));
            if quiet {
                hints.insert("suppress-sound", Value::from(true));
                hints.insert("urgency", Value::from(0u8));
            }
            bounded(self.proxy.call(
                "Notify",
                &(
                    "Air",
                    replaces,
                    APP_ID,
                    alert.title.as_str(),
                    body.as_str(),
                    Vec::<&str>::new(),
                    hints,
                    -1i32,
                ),
            ))
            .await
        }

        async fn close(&self, id: u32) -> zbus::Result<()> {
            bounded(self.proxy.call("CloseNotification", &(id,))).await
        }
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
        #[test]
        fn peer_text_never_becomes_markup() {
            assert_eq!(
                super::escape_markup(r#"<img src="http://x/t.png"> & <a href='x'>"#),
                "&lt;img src=&quot;http://x/t.png&quot;&gt; &amp; &lt;a href=&#39;x&#39;&gt;"
            );
        }
    }
}
