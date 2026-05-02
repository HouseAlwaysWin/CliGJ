//! Xterm-like keyboard encoding for bytes written to ConPTY.
//! Reference: XTerm control sequences (modifyOtherKeys / CSI), common TUI expectations.
//!
//! Modifier bitmask (must match Slint `pty_mod_bits`):
//! - `1` Ctrl, `2` Shift, `4` Alt, `8` Meta

pub const MOD_CTRL: u32 = 1;
pub const MOD_SHIFT: u32 = 2;
pub const MOD_ALT: u32 = 4;
pub const MOD_META: u32 = 8;

/// Build mask from Slint modifier booleans (same order as `pty_mod` in `gj_prompt_drop_zone.slint`).
#[allow(dead_code)] // Public for future IPC / tests call `mod_bits` only under `cfg(test)` in some toolchains.
#[must_use]
pub const fn mod_bits(ctrl: bool, shift: bool, alt: bool, meta: bool) -> u32 {
    (if ctrl { MOD_CTRL } else { 0 })
        | (if shift { MOD_SHIFT } else { 0 })
        | (if alt { MOD_ALT } else { 0 })
        | (if meta { MOD_META } else { 0 })
}

/// Map Slint / platform key names and Unicode arrow glyphs to tokens understood by
/// [`encode_for_pty`]. (Windows often reports arrows as `↑↓←→`, not the string `UpArrow`.)
#[must_use]
pub fn normalize_tty_key_token(key: &str) -> &str {
    match key {
        // Unicode arrows (common for UI toolkits)
        "\u{2191}" => "UpArrow",
        "\u{2193}" => "DownArrow",
        "\u{2190}" => "LeftArrow",
        "\u{2192}" => "RightArrow",
        // Ignore bare modifier key events (Windows VK_SHIFT=0x10, VK_CONTROL=0x11, VK_MENU=0x12)
        // and string names that some windowing backends leak into text events.
        "\u{10}" | "\u{11}" | "\u{12}" | "\u{14}" | "Shift" | "Control" | "Alt" | "Meta"
        | "Super" | "OS" => "",
        _ => key,
    }
}

/// Encode one logical key for TTY/ConPTY input.
/// `key` is either a **token** (`Return`, `UpArrow`, …) or UTF-8 **text** (typed character or paste).
#[must_use]
pub fn encode_for_pty(mod_mask: u32, key: &str) -> Option<Vec<u8>> {
    let key = normalize_tty_key_token(key);
    if key.is_empty() {
        return None;
    }

    // Known non-special multi-byte: treat whole string as UTF-8 (paste / IME commit).
    if is_known_named_key(key) {
        return encode_named_or_text(mod_mask, key);
    }
    if key.chars().nth(1).is_some() {
        return encode_utf8_text(mod_mask, key);
    }

    // Single Unicode scalar (or lone byte).
    encode_named_or_text(mod_mask, key)
}

fn is_known_named_key(key: &str) -> bool {
    matches!(
        key,
        "Return"
            | "Tab"
            | "Backspace"
            | "Escape"
            | "UpArrow"
            | "DownArrow"
            | "LeftArrow"
            | "RightArrow"
            | "Home"
            | "End"
            | "PageUp"
            | "PageDown"
            | "Delete"
            | "Insert"
    )
}

fn encode_named_or_text(mod_mask: u32, key: &str) -> Option<Vec<u8>> {
    // Ctrl+A .. Ctrl+Z (and a few punctuation) as C0 controls.
    if mod_mask & MOD_CTRL != 0 && key.len() == 1 {
        let b = key.as_bytes()[0];
        if matches!(b, b'a'..=b'z') {
            return Some(vec![b - b'a' + 1]);
        }
        if matches!(b, b'A'..=b'Z') {
            return Some(vec![b - b'A' + 1]);
        }
        if b == b'[' {
            return Some(vec![0x1b]);
        }
        if b == b'\\' {
            return Some(vec![0x1c]);
        }
        if b == b']' {
            return Some(vec![0x1d]);
        }
        if b == b'^' {
            return Some(vec![0x1e]);
        }
        if b == b'_' || b == b'-' {
            return Some(vec![0x1f]);
        }
    }

    // Alt+printable: ESC + char (8-bit meta style; widely accepted by TUIs).
    if mod_mask & MOD_ALT != 0 && key.chars().count() == 1 {
        let mut ch = [0u8; 4];
        let s = key.chars().next()?;
        let t = s.encode_utf8(&mut ch);
        let mut out = Vec::with_capacity(1 + t.len());
        out.push(0x1b);
        out.extend_from_slice(t.as_bytes());
        return Some(out);
    }

    match key {
        "Return" | "\n" | "\u{000d}" => {
            let _ = mod_mask & MOD_SHIFT;
            // CRLF is common on Windows consoles; many CLIs accept CR alone for the main prompt.
            Some(vec![b'\r'])
        }
        "Tab" => {
            if mod_mask & MOD_SHIFT != 0 {
                // Back-tab (Shift+Tab).
                Some(vec![0x1b, b'[', b'Z'])
            } else {
                Some(vec![b'\t'])
            }
        }
        "Backspace" => Some(vec![0x7f]),
        "Escape" => Some(vec![0x1b]),
        "Insert" => Some(vec![0x1b, b'[', b'2', b'~']),
        "Delete" => {
            let m = xterm_mod_param(mod_mask);
            if m == 1 {
                Some(vec![0x1b, b'[', b'3', b'~'])
            } else {
                Some(format!("\x1b[3;{}~", m).into_bytes())
            }
        }
        "Home" => {
            let m = xterm_mod_param(mod_mask);
            if m == 1 {
                Some(vec![0x1b, b'[', b'H'])
            } else {
                Some(format!("\x1b[1;{}H", m).into_bytes())
            }
        }
        "End" => {
            let m = xterm_mod_param(mod_mask);
            if m == 1 {
                Some(vec![0x1b, b'[', b'F'])
            } else {
                Some(format!("\x1b[1;{}F", m).into_bytes())
            }
        }
        "PageUp" => page_key(mod_mask, b'5'),
        "PageDown" => page_key(mod_mask, b'6'),
        "UpArrow" => arrow(mod_mask, b'A'),
        "DownArrow" => arrow(mod_mask, b'B'),
        "RightArrow" => arrow(mod_mask, b'C'),
        "LeftArrow" => arrow(mod_mask, b'D'),
        _ => encode_utf8_text(mod_mask, key),
    }
}

fn encode_utf8_text(_mod_mask: u32, key: &str) -> Option<Vec<u8>> {
    Some(key.as_bytes().to_vec())
}

/// Xterm modifier parameter for CSI `1;m` forms (simplified).
fn xterm_mod_param(mod_mask: u32) -> u8 {
    let shift = mod_mask & MOD_SHIFT != 0;
    let alt = mod_mask & MOD_ALT != 0;
    let ctrl = mod_mask & MOD_CTRL != 0;
    let meta = mod_mask & MOD_META != 0;
    match (ctrl, alt, shift, meta) {
        (false, false, false, false) => 1,
        (false, false, true, false) => 2, // Shift
        (false, true, false, false) => 3, // Alt
        (false, true, true, false) => 4,  // Shift+Alt
        (true, false, false, false) => 5, // Ctrl
        (true, false, true, false) => 6,  // Ctrl+Shift
        (true, true, false, false) => 7,  // Ctrl+Alt
        (true, true, true, false) => 8,   // Ctrl+Alt+Shift
        (false, false, false, true)
        | (false, false, true, true)
        | (false, true, false, true)
        | (false, true, true, true)
        | (true, false, false, true)
        | (true, false, true, true)
        | (true, true, false, true)
        | (true, true, true, true) => 9, // Meta (best-effort)
    }
}

fn arrow(mod_mask: u32, dir: u8) -> Option<Vec<u8>> {
    let m = xterm_mod_param(mod_mask);
    if m == 1 {
        return Some(vec![0x1b, b'[', dir]);
    }
    // CSI `ESC [ 1 ; Ps A` — cursor keys with modifier (xterm).
    Some(format!("\x1b[1;{}{}", m, char::from(dir)).into_bytes())
}

fn page_key(mod_mask: u32, num: u8) -> Option<Vec<u8>> {
    let m = xterm_mod_param(mod_mask);
    if m == 1 {
        return Some(vec![0x1b, b'[', num, b'~']);
    }
    let ch = char::from_u32(u32::from(num)).unwrap();
    Some(format!("\x1b[{ch};{}~", m).into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_c() {
        let v = encode_for_pty(MOD_CTRL, "c").unwrap();
        assert_eq!(v, vec![0x03]);
    }

    #[test]
    fn shift_tab() {
        let v = encode_for_pty(MOD_SHIFT, "Tab").unwrap();
        assert_eq!(v, vec![0x1b, b'[', b'Z']);
    }

    #[test]
    fn plain_arrow() {
        let v = encode_for_pty(0, "UpArrow").unwrap();
        assert_eq!(v, vec![0x1b, b'[', b'A']);
    }

    #[test]
    fn unicode_arrow_glyph_same_as_up_arrow_token() {
        let expected = encode_for_pty(0, "UpArrow").unwrap();
        assert_eq!(encode_for_pty(0, "\u{2191}").unwrap(), expected);
        assert_eq!(
            encode_for_pty(0, "\u{2193}").unwrap(),
            encode_for_pty(0, "DownArrow").unwrap()
        );
    }

    #[test]
    fn mod_bits_public() {
        assert_eq!(mod_bits(true, false, false, false), MOD_CTRL);
        assert_eq!(mod_bits(false, true, true, false), MOD_SHIFT | MOD_ALT);
    }

    #[test]
    fn ctrl_up_arrow() {
        let v = encode_for_pty(MOD_CTRL, "UpArrow").unwrap();
        assert_eq!(v, b"\x1b[1;5A");
    }
}
