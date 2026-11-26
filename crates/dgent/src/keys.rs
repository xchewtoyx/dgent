//! Translate crossterm key events into the bytes a terminal would send.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn encode(key: KeyEvent, app_cursor: bool) -> Vec<u8> {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let mut out = if alt { vec![0x1b] } else { vec![] };
    let csi = |c: char| {
        if app_cursor {
            format!("\x1bO{c}")
        } else {
            format!("\x1b[{c}")
        }
    };
    let s: String = match key.code {
        KeyCode::Char(c) if ctrl => {
            let b = c.to_ascii_lowercase() as u8;
            match b {
                b'a'..=b'z' => (b - b'a' + 1) as char,
                b' ' | b'@' => '\0',
                b'[' | b'3' => '\x1b',
                // crossterm reports raw 0x1c..=0x1f as Ctrl-4..=Ctrl-7.
                b'\\' | b'4' => '\x1c',
                b']' | b'5' => '\x1d',
                b'^' | b'6' => '\x1e',
                b'_' | b'/' | b'7' => '\x1f',
                _ => c,
            }
            .to_string()
        }
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "\r".into(),
        KeyCode::Backspace => "\x7f".into(),
        KeyCode::Tab => "\t".into(),
        KeyCode::BackTab => "\x1b[Z".into(),
        KeyCode::Esc => "\x1b".into(),
        KeyCode::Up => csi('A'),
        KeyCode::Down => csi('B'),
        KeyCode::Right => csi('C'),
        KeyCode::Left => csi('D'),
        KeyCode::Home => csi('H'),
        KeyCode::End => csi('F'),
        KeyCode::Insert => "\x1b[2~".into(),
        KeyCode::Delete => "\x1b[3~".into(),
        KeyCode::PageUp => "\x1b[5~".into(),
        KeyCode::PageDown => "\x1b[6~".into(),
        KeyCode::F(n @ 1..=4) => format!("\x1bO{}", (b'P' + n - 1) as char),
        KeyCode::F(n) => match n {
            5 => "\x1b[15~",
            6 => "\x1b[17~",
            7 => "\x1b[18~",
            8 => "\x1b[19~",
            9 => "\x1b[20~",
            10 => "\x1b[21~",
            11 => "\x1b[23~",
            12 => "\x1b[24~",
            _ => "",
        }
        .into(),
        _ => String::new(),
    };
    out.extend_from_slice(s.as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn encodes_common_keys() {
        assert_eq!(
            encode(k(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            b"\x03"
        );
        assert_eq!(
            encode(k(KeyCode::Char('x'), KeyModifiers::ALT), false),
            b"\x1bx"
        );
        for (c, b) in [
            ('^', 0x1e),
            ('6', 0x1e),
            ('_', 0x1f),
            ('7', 0x1f),
            ('4', 0x1c),
        ] {
            assert_eq!(
                encode(k(KeyCode::Char(c), KeyModifiers::CONTROL), false),
                [b]
            );
        }
        assert_eq!(encode(k(KeyCode::Enter, KeyModifiers::NONE), false), b"\r");
        assert_eq!(encode(k(KeyCode::Up, KeyModifiers::NONE), false), b"\x1b[A");
        assert_eq!(encode(k(KeyCode::Up, KeyModifiers::NONE), true), b"\x1bOA");
        assert_eq!(
            encode(k(KeyCode::F(1), KeyModifiers::NONE), false),
            b"\x1bOP"
        );
    }
}
