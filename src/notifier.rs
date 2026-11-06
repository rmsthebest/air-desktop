//! Shows alerts as desktop notifications. On Linux this talks to
//! `org.freedesktop.Notifications` over the session bus, as upstream's
//! direct fallback does. Upstream prefers the notification portal, but
//! that needs a registered app ID and desktop file, which an unpackaged
//! build doesn't have.
//!
//! Each chat has at most one notification on screen: a new alert replaces
//! it, and a dismissal closes it.

use tokio::sync::mpsc;

use crate::session::alerts::Notice;

/// Runs until the session drops its sender.
#[cfg(target_os = "linux")]
pub async fn run(mut notices: mpsc::Receiver<Notice>) {
    use std::collections::HashMap;

    use tracing::warn;

    // Connected on the first alert, and again after a failure.
    let mut bus: Option<linux::Bus> = None;
    // The server's id of each chat's notification.
    let mut shown: HashMap<aircoreclient::ChatId, u32> = HashMap::new();
    while let Some(notice) = notices.recv().await {
        match notice {
            Notice::Show(alert) => {
                if bus.is_none() {
                    bus = linux::Bus::connect()
                        .await
                        .inspect_err(|err| warn!("desktop notifications unavailable: {err}"))
                        .ok();
                }
                let Some(connected) = &bus else { continue };
                let replaces = shown.get(&alert.chat).copied().unwrap_or(0);
                match connected.notify(replaces, &alert).await {
                    Ok(id) => {
                        shown.insert(alert.chat, id);
                    }
                    Err(err) => {
                        warn!("failed to show a desktop notification: {err}");
                        bus = None;
                    }
                }
            }
            Notice::Dismiss(chat) => {
                if let (Some(id), Some(connected)) = (shown.remove(&chat), &bus) {
                    // Fails if it is already gone, which is fine.
                    _ = connected.close(id).await;
                }
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub async fn run(mut notices: mpsc::Receiver<Notice>) {
    // Not implemented on this platform yet; drain so senders never block.
    while notices.recv().await.is_some() {}
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;

    use zbus::zvariant::Value;
    use zbus::{Connection, Proxy};

    use crate::session::alerts::Alert;

    /// Matches the window's app id, so the desktop can group them.
    const APP_ID: &str = "air-desktop";

    pub(super) struct Bus {
        proxy: Proxy<'static>,
        /// The server renders a subset of HTML in bodies.
        markup: bool,
    }

    impl Bus {
        pub(super) async fn connect() -> zbus::Result<Self> {
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
            self.proxy
                .call(
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
                )
                .await
        }

        pub(super) async fn close(&self, id: u32) -> zbus::Result<()> {
            self.proxy.call("CloseNotification", &(id,)).await
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
