//! Translate crossterm key events into the bytes a terminal would send.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn encode(key: KeyEvent, app_cursor: bool) -> Vec<u8> {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // xterm modifier parameter: 1 + shift + 2*alt + 4*ctrl.
    let m = 1 + key.modifiers.contains(KeyModifiers::SHIFT) as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let csi = |c: char| {
        if m > 1 {
            format!("\x1b[1;{m}{c}")
        } else if app_cursor {
            format!("\x1bO{c}")
        } else {
            format!("\x1b[{c}")
        }
    };
    // `ESC[n~` keys carry modifiers as `ESC[n;m~`; F1-F4 (`ESC O P`) as `ESC[1;mP`.
    let tilde = |n: u8| {
        if m > 1 {
            format!("\x1b[{n};{m}~")
        } else {
            format!("\x1b[{n}~")
        }
    };
    let ss3 = |c: char| {
        if m > 1 {
            format!("\x1b[1;{m}{c}")
        } else {
            format!("\x1bO{c}")
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
        // screen terminfo: khome=\E[1~, kend=\E[4~ (modified forms stay xterm-style).
        KeyCode::Home if m == 1 => "\x1b[1~".into(),
        KeyCode::End if m == 1 => "\x1b[4~".into(),
        KeyCode::Home => csi('H'),
        KeyCode::End => csi('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => ss3((b'P' + n - 1) as char),
        KeyCode::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5]),
        _ => String::new(),
    };
    // Alt is an ESC prefix, except where the sequence already carries the modifier.
    let mut out = if alt && !(s.starts_with("\x1b[") && s.contains(';')) {
        vec![0x1b]
    } else {
        vec![]
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
        assert_eq!(
            encode(k(KeyCode::Right, KeyModifiers::CONTROL), false),
            b"\x1b[1;5C"
        );
        assert_eq!(
            encode(k(KeyCode::Left, KeyModifiers::ALT), true),
            b"\x1b[1;3D"
        );
        assert_eq!(
            encode(k(KeyCode::Delete, KeyModifiers::CONTROL), false),
            b"\x1b[3;5~"
        );
        assert_eq!(
            encode(k(KeyCode::F(1), KeyModifiers::SHIFT), false),
            b"\x1b[1;2P"
        );
        assert_eq!(
            encode(k(KeyCode::F(5), KeyModifiers::ALT), false),
            b"\x1b[15;3~"
        );
        assert_eq!(
            encode(k(KeyCode::F(5), KeyModifiers::NONE), false),
            b"\x1b[15~"
        );
        assert_eq!(encode(k(KeyCode::Enter, KeyModifiers::NONE), false), b"\r");
        assert_eq!(encode(k(KeyCode::Up, KeyModifiers::NONE), false), b"\x1b[A");
        assert_eq!(encode(k(KeyCode::Up, KeyModifiers::NONE), true), b"\x1bOA");
        assert_eq!(
            encode(k(KeyCode::F(1), KeyModifiers::NONE), false),
            b"\x1bOP"
        );
    }
}
