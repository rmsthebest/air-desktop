//! The latest log lines, in memory, for the logs page. Never on disk: logs
//! can name contacts. They end with the app, and copying them is the user's
//! explicit act.

use std::collections::VecDeque;
use std::io;
use std::sync::Mutex;

const MAX_LINES: usize = 1000;
const MAX_LINE_BYTES: usize = 400;

static LATEST: Mutex<Buffer> = Mutex::new(Buffer {
    lines: VecDeque::new(),
});

struct Buffer {
    lines: VecDeque<String>,
}

impl Buffer {
    /// One event is one line, whatever a logged value holds: a name with a
    /// line break in it can't pass for another event.
    fn push(&mut self, event: &str) {
        let line = bounded(event.trim_end());
        if line.is_empty() {
            return;
        }
        if self.lines.len() == MAX_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }
}

/// One bounded line of plain text: what logs show can't reorder or hide
/// itself, as for peer text elsewhere.
fn bounded(line: &str) -> String {
    let mut end = line.len().min(MAX_LINE_BYTES);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end]
        .chars()
        .map(|c| {
            if c.is_whitespace() || c.is_control() {
                ' '
            } else {
                c
            }
        })
        .filter(|c| !crate::model::is_invisible(*c))
        .collect()
}

/// The lines kept, oldest first.
pub fn latest() -> Vec<String> {
    let buffer = LATEST.lock().unwrap_or_else(|err| err.into_inner());
    buffer.lines.iter().cloned().collect()
}

/// Forgets the lines kept: they are about an account that is going away.
pub fn clear() {
    let mut buffer = LATEST.lock().unwrap_or_else(|err| err.into_inner());
    buffer.lines.clear();
}

/// The layer that keeps the lines.
pub fn layer<S>() -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(|| Writer)
}

/// Where the log layer writes: each event arrives as one write.
pub struct Writer;

impl io::Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        LATEST
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .push(&String::from_utf8_lossy(bytes));
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer() -> Buffer {
        Buffer {
            lines: VecDeque::new(),
        }
    }

    #[test]
    fn it_keeps_only_the_newest_lines() {
        let mut buffer = buffer();
        for n in 0..MAX_LINES + 5 {
            buffer.push(&format!("line {n}\n"));
        }
        assert_eq!(buffer.lines.len(), MAX_LINES);
        assert_eq!(buffer.lines.front().unwrap(), "line 5");
    }

    #[test]
    fn a_line_is_bounded_and_plain() {
        let mut buffer = buffer();
        // The cut lands inside a character.
        buffer.push(&format!("a{}", "é".repeat(MAX_LINE_BYTES)));
        assert_eq!(buffer.lines[0].len(), MAX_LINE_BYTES - 1);
        buffer.push("a\u{1b}[31mb\nnext\u{2028}event\n\n");
        assert_eq!(buffer.lines[1], "a [31mb next event");
        buffer.push("\u{202e}gnp.exe\u{200b}\n");
        assert_eq!(buffer.lines[2], "gnp.exe");
        buffer.push("\n");
        assert_eq!(buffer.lines.len(), 3);
    }

    /// One event through the layer as `main` installs it: one line, no
    /// terminal colours.
    #[test]
    fn the_layer_keeps_one_plain_line_per_event() {
        use tracing_subscriber::layer::SubscriberExt as _;
        const MARK: &str = "the_layer_keeps_one_plain_line_per_event";
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            // In the message: other fields are written escaped.
            tracing::warn!("{MARK}\nsecond");
        });
        let lines: Vec<_> = latest()
            .into_iter()
            .filter(|line| line.contains(MARK))
            .collect();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("second"), "{lines:?}");
        assert!(lines[0].contains("WARN"), "{lines:?}");
        assert!(!lines[0].contains('['), "no colour codes: {lines:?}");
    }
}
