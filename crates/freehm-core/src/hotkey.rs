//! Platform-neutral global shortcut description, parsing and formatting.
//!
//! Shortcuts are stored as text ("Ctrl+Shift+O") so the configuration file stays readable.
//! Platform layers translate a [`Hotkey`] into `RegisterHotKey` modifiers/virtual keys,
//! X11 keysyms, or an XDG portal trigger description.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// 'A'..='Z'
    Letter(char),
    /// '0'..='9'
    Digit(char),
    /// F1..=F24
    F(u8),
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Pause,
    ScrollLock,
    PrintScreen,
    Backquote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// Windows key / Super.
    pub meta: bool,
    pub key: Key,
}

impl Hotkey {
    /// Parse "Ctrl+Shift+O", "alt + f10", "Super+Home", ... (case-insensitive).
    pub fn parse(s: &str) -> Result<Hotkey, String> {
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        if parts.iter().any(|p| p.is_empty()) {
            return Err("empty key name (stray '+')".into());
        }
        let (mut ctrl, mut shift, mut alt, mut meta) = (false, false, false, false);
        let mut key = None;
        for p in parts {
            let l = p.to_ascii_lowercase();
            let flag = match l.as_str() {
                "ctrl" | "control" => Some(&mut ctrl),
                "shift" => Some(&mut shift),
                "alt" => Some(&mut alt),
                "win" | "super" | "meta" | "cmd" => Some(&mut meta),
                _ => None,
            };
            if let Some(f) = flag {
                if *f {
                    return Err(format!("modifier '{p}' given twice"));
                }
                *f = true;
                continue;
            }
            if key.is_some() {
                return Err("only one non-modifier key is allowed".into());
            }
            key = Some(parse_key(&l).ok_or_else(|| format!("unknown key '{p}'"))?);
        }
        let key = key.ok_or("no key given, only modifiers")?;
        let hk = Hotkey { ctrl, shift, alt, meta, key };
        // Bare letters/digits would break typing in every application; function keys and
        // the navigation cluster are acceptable without modifiers.
        let has_mod = ctrl || alt || meta;
        if matches!(key, Key::Letter(_) | Key::Digit(_) | Key::Backquote) && !has_mod {
            return Err("letters, digits and ` need Ctrl, Alt or Win/Super".into());
        }
        Ok(hk)
    }
}

fn parse_key(l: &str) -> Option<Key> {
    let mut chars = l.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return match c {
            'a'..='z' => Some(Key::Letter(c.to_ascii_uppercase())),
            '0'..='9' => Some(Key::Digit(c)),
            '`' => Some(Key::Backquote),
            _ => None,
        };
    }
    if let Some(n) = l.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        return (1..=24).contains(&n).then_some(Key::F(n));
    }
    Some(match l {
        "insert" | "ins" => Key::Insert,
        "delete" | "del" => Key::Delete,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "pause" => Key::Pause,
        "scrolllock" => Key::ScrollLock,
        "printscreen" | "prtsc" => Key::PrintScreen,
        "backquote" | "grave" => Key::Backquote,
        _ => return None,
    })
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Letter(c) | Key::Digit(c) => write!(f, "{c}"),
            Key::F(n) => write!(f, "F{n}"),
            Key::Insert => f.write_str("Insert"),
            Key::Delete => f.write_str("Delete"),
            Key::Home => f.write_str("Home"),
            Key::End => f.write_str("End"),
            Key::PageUp => f.write_str("PageUp"),
            Key::PageDown => f.write_str("PageDown"),
            Key::Pause => f.write_str("Pause"),
            Key::ScrollLock => f.write_str("ScrollLock"),
            Key::PrintScreen => f.write_str("PrintScreen"),
            Key::Backquote => f.write_str("`"),
        }
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        if self.meta {
            f.write_str("Win+")?;
        }
        write!(f, "{}", self.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default() {
        let h = Hotkey::parse("Ctrl+Shift+O").unwrap();
        assert!(h.ctrl && h.shift && !h.alt && !h.meta);
        assert_eq!(h.key, Key::Letter('O'));
        assert_eq!(h.to_string(), "Ctrl+Shift+O");
    }

    #[test]
    fn case_and_spacing_insensitive_and_normalised() {
        let h = Hotkey::parse(" shift + ctrl + o ").unwrap();
        assert_eq!(h.to_string(), "Ctrl+Shift+O");
        assert_eq!(Hotkey::parse("alt+F10").unwrap().key, Key::F(10));
        assert_eq!(Hotkey::parse("super+home").unwrap().to_string(), "Win+Home");
    }

    #[test]
    fn function_keys_alone_are_allowed() {
        assert!(Hotkey::parse("F12").is_ok());
        assert!(Hotkey::parse("Shift+F3").is_ok());
    }

    #[test]
    fn rejects_bad_input() {
        for bad in ["", "Ctrl+", "Ctrl+Shift", "O", "Shift+O", "Ctrl+Ctrl+O", "Ctrl+A+B", "Ctrl+F25", "Ctrl+Banana"] {
            assert!(Hotkey::parse(bad).is_err(), "{bad} should be rejected");
        }
    }
}
