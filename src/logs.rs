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
    fn push(&mut self, text: &str) {
        for line in text.lines().filter(|line| !line.is_empty()) {
            if self.lines.len() == MAX_LINES {
                self.lines.pop_front();
            }
            self.lines.push_back(bounded(line));
        }
    }
}

/// One bounded line of plain text.
fn bounded(line: &str) -> String {
    let mut end = line.len().min(MAX_LINE_BYTES);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end]
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The lines kept, oldest first.
pub fn latest() -> Vec<String> {
    let buffer = LATEST.lock().unwrap_or_else(|err| err.into_inner());
    buffer.lines.iter().cloned().collect()
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
        buffer.push(&"é".repeat(MAX_LINE_BYTES));
        assert!(buffer.lines[0].len() <= MAX_LINE_BYTES);
        buffer.push("a\u{1b}[31mb\nsecond\n\n");
        assert_eq!(buffer.lines[1], "a [31mb");
        assert_eq!(buffer.lines[2], "second");
        assert_eq!(buffer.lines.len(), 3);
    }
}
