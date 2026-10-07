use std::collections::VecDeque;
use std::io::Write;
use std::sync::Mutex;

const LOG_CAP: usize = 2000;

static RECENT_LOGS: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// `tracing_subscriber`'s `MakeWriter` closure: forwards every formatted line to stdout as
/// before, and keeps a capped copy in memory so the web UI can show it without the binary
/// needing to manage its own log file/rotation.
pub fn tee_writer() -> TeeWriter {
    TeeWriter
}

pub struct TeeWriter;

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::stdout().write_all(buf)?;
        if let Ok(text) = std::str::from_utf8(buf) {
            let mut log = RECENT_LOGS.lock().expect("log buffer mutex poisoned");
            for line in text.lines() {
                log.push_back(strip_ansi(line));
                if log.len() > LOG_CAP {
                    log.pop_front();
                }
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stdout().flush()
    }
}

/// Drop ANSI SGR escape sequences (`tracing_subscriber`'s default colored output) so the web
/// UI's `<pre>` shows plain text; `docker logs` still gets the original, color-coded bytes.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.clone().next() == Some('[') {
            chars.next(); // consume '['
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The most recent `max_lines` log lines, oldest first.
pub fn recent_lines(max_lines: usize) -> Vec<String> {
    let log = RECENT_LOGS.lock().expect("log buffer mutex poisoned");
    let skip = log.len().saturating_sub(max_lines);
    log.iter().skip(skip).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_sgr_sequences_but_keeps_text() {
        assert_eq!(
            strip_ansi("\u{1b}[2m2026-10-07\u{1b}[0m \u{1b}[32mINFO\u{1b}[0m request"),
            "2026-10-07 INFO request"
        );
        assert_eq!(strip_ansi("plain text, no codes"), "plain text, no codes");
    }
}
