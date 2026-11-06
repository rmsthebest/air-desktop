//! Shows alerts as desktop notifications. On Linux this talks to
//! `org.freedesktop.Notifications` over the session bus, as upstream's
//! direct fallback does. Upstream prefers the notification portal, but
//! that needs a registered app ID and desktop file, which an unpackaged
//! build doesn't have.
//!
//! Each chat has at most one notification on screen: a new alert replaces
//! it, and a dismissal closes it.

use tokio::sync::{mpsc, watch};

use crate::session::alerts::{Alert, Attention, Dismiss};

/// Runs until the session drops its senders. Dismissals go first, and an
/// alert for the chat the user is looking at by the time it is shown is
/// dropped (it may have been built just before they opened it).
pub async fn run(
    mut shows: mpsc::Receiver<Alert>,
    mut dismissals: mpsc::UnboundedReceiver<Dismiss>,
    attention: watch::Receiver<Attention>,
) {
    let mut notifier = Notifier::default();
    loop {
        tokio::select! {
            biased;
            Some(dismiss) = dismissals.recv() => notifier.dismiss(dismiss).await,
            Some(alert) = shows.recv() => {
                if attention.borrow().watched() != Some(alert.chat) {
                    notifier.show(alert).await;
                }
            }
            else => return,
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct Notifier {
    /// Connected on the first alert, and again after a failure.
    bus: Option<linux::Bus>,
    /// When connecting last failed: retried after [`RETRY`], warned once.
    failed_at: Option<tokio::time::Instant>,
    /// The server's id of each chat's notification.
    shown: std::collections::HashMap<aircoreclient::ChatId, u32>,
}

/// How long a missing notification service is left alone.
#[cfg(target_os = "linux")]
const RETRY: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(target_os = "linux")]
impl Notifier {
    async fn show(&mut self, alert: Alert) {
        use tracing::{info, warn};

        if self.bus.is_none() {
            if self.failed_at.is_some_and(|at| at.elapsed() < RETRY) {
                return;
            }
            match linux::Bus::connect().await {
                Ok(bus) => {
                    if self.failed_at.take().is_some() {
                        info!("desktop notifications available again");
                    }
                    self.bus = Some(bus);
                }
                Err(err) => {
                    if self
                        .failed_at
                        .replace(tokio::time::Instant::now())
                        .is_none()
                    {
                        warn!("desktop notifications unavailable: {err}");
                    }
                    return;
                }
            }
        }
        let Some(bus) = &self.bus else { return };
        let replaces = self.shown.get(&alert.chat).copied().unwrap_or(0);
        match bus.notify(replaces, &alert).await {
            Ok(id) => {
                self.shown.insert(alert.chat, id);
            }
            Err(err) => {
                warn!("failed to show a desktop notification: {err}");
                self.bus = None;
            }
        }
    }

    async fn dismiss(&mut self, dismiss: Dismiss) {
        let ids: Vec<u32> = match dismiss {
            Dismiss::Chat(chat) => self.shown.remove(&chat).into_iter().collect(),
            Dismiss::All => self.shown.drain().map(|(_, id)| id).collect(),
        };
        if let Some(bus) = &self.bus {
            for id in ids {
                // Fails if it is already gone, which is fine.
                _ = bus.close(id).await;
            }
        }
    }
}

/// Not implemented on this platform yet.
#[cfg(not(target_os = "linux"))]
#[derive(Default)]
struct Notifier;

#[cfg(not(target_os = "linux"))]
impl Notifier {
    async fn show(&mut self, _: Alert) {}
    async fn dismiss(&mut self, _: Dismiss) {}
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;

    use std::time::Duration;

    use zbus::zvariant::Value;
    use zbus::{Connection, Proxy};

    use crate::session::alerts::Alert;

    /// Matches the window's app id, so the desktop can group them.
    const APP_ID: &str = "air-desktop";

    /// A hung notification service mustn't stall the notifier.
    const CALL_TIMEOUT: Duration = Duration::from_secs(5);

    async fn bounded<T>(call: impl Future<Output = zbus::Result<T>>) -> zbus::Result<T> {
        tokio::time::timeout(CALL_TIMEOUT, call)
            .await
            .map_err(|_| zbus::Error::Failure("timed out".into()))?
    }

    pub(super) struct Bus {
        proxy: Proxy<'static>,
        /// The server renders a subset of HTML in bodies.
        markup: bool,
    }

    impl Bus {
        pub(super) async fn connect() -> zbus::Result<Self> {
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

        pub(super) async fn notify(&self, replaces: u32, alert: &Alert) -> zbus::Result<u32> {
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

        pub(super) async fn close(&self, id: u32) -> zbus::Result<()> {
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
