//! Terminal theme detection, ported from OpenTUI's renderer theme mode:
//! query the terminal's background color over OSC 11 and infer dark or
//! light from its brightness.

use std::io::{Read, Write};

/// How long the background query waits for a reply.
const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(150);

/// Ask the terminal for its background color and judge its brightness.
///
/// Returns `None` when the terminal does not answer: not a TTY, no reply
/// within the timeout, or an unparseable one.
pub fn detect_background() -> Option<u32> {
    let mut tty = std::fs::File::create("/dev/tty").ok()?;
    // OSC 11 with an empty color asks "what is your default background?".
    tty.write_all(b"\x1b]11;?\x1b\\").ok()?;
    tty.flush().ok()?;

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = match std::fs::File::open("/dev/tty") {
            Ok(file) => file,
            Err(_) => return,
        };
        let mut buf = [0u8; 64];
        match reader.read(&mut buf) {
            Ok(n) if n > 0 => {
                let _ = tx.send(buf[..n].to_vec());
            }
            _ => {}
        }
    });
    let reply = rx.recv_timeout(QUERY_TIMEOUT).ok()?;
    parse_background(&String::from_utf8_lossy(&reply))
}

/// Parse an OSC 11 reply like `\x1b]11;rgb:1a20/2833/4a5f\x1b\\` (the
/// components may be 1 to 4 hex digits each).
pub fn parse_background(reply: &str) -> Option<u32> {
    let body = reply.split("rgb:").nth(1)?;
    let body = body
        .split(['\u{1b}', '\u{7}'])
        .next()?
        .trim();
    let mut parts = body.split('/');
    let r = component(parts.next()?)?;
    let g = component(parts.next()?)?;
    let b = component(parts.next()?)?;
    Some((r << 16) | (g << 8) | b)
}

/// One OSC color component: 1-4 hex digits, scaled to 8 bits.
fn component(raw: &str) -> Option<u32> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 4 || !raw.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(raw, 16).ok()?;
    let max = (1 << (4 * raw.len())) - 1;
    Some((value * 255 + max / 2) / max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_osc_replies() {
        assert_eq!(parse_background("\x1b]11;rgb:1a20/2833/4a5f\x1b\\"), Some(0x1a284a));
        assert_eq!(parse_background("\x1b]11;rgb:ffff/ffff/ffff\x1b\\"), Some(0xffffff));
        assert_eq!(parse_background("\x1b]11;rgb:0000/0000/0000\x1b\\"), Some(0x000000));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_background("not an osc reply"), None);
        assert_eq!(parse_background("\x1b]10;rgb:1a2b3c\x1b\\"), None);
        assert_eq!(parse_background("\x1b]11;rgb:zzzz/0000/0000\x1b\\"), None);
    }
}
