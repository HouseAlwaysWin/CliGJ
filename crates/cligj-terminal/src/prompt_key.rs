//! Composer / TTY prompt key routing.
//!
//! Text-editing keys (printable chars, Backspace, Delete, horizontal arrows,
//! Home, End) are **rejected** so the Slint TextEdit handles them locally.
//! The 20 ms composer-sync timer mirrors the resulting text to ConPTY.
//!
//! Terminal-specific keys (Up/Down for shell history, Tab for completion,
//! Escape, Ctrl+C, PageUp/PageDown) go **directly** to the PTY.

use super::key_encoding::{MOD_ALT, MOD_CTRL, MOD_SHIFT, normalize_tty_key_token};

/// What to do for one `FocusScope` `capture-key-pressed` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKeyAction {
    /// Let `TextEdit` handle the key (insert character, etc.).
    Reject,
    Submit,
    /// Open the Ctrl+Space workspace file picker.
    OpenFilePicker,
    /// Encode with `key_encoding::encode_for_pty(mod_mask, …)` then write to ConPTY.
    PtyKey(String),
}

#[must_use]
pub fn route_prompt_key(mod_mask: u32, key: &str, _shift: bool) -> PromptKeyAction {
    let key = normalize_tty_key_token(key);

    // 1. Ctrl+Space -> open file picker
    //    On Windows, Ctrl+Space produces NUL (\0) via the terminal convention.
    if (mod_mask & MOD_CTRL != 0
        && (mod_mask & (MOD_ALT | MOD_SHIFT)) == 0
        && (key == " " || key == "Space"))
        || key == "\0"
    {
        return PromptKeyAction::OpenFilePicker;
    }

    // 2. Enter always submits
    if is_enter_key(key) {
        return PromptKeyAction::Submit;
    }

    // 3. Escape -> PTY
    if key == "Escape" {
        return pty("Escape");
    }

    // 4. Alt combos -> PTY (word-level movement, etc.)
    if mod_mask & MOD_ALT != 0 {
        match key {
            "UpArrow" | "DownArrow" | "RightArrow" | "LeftArrow" => return pty(key),
            _ => {}
        }
    }

    // 5. Vertical navigation / paging -> PTY (shell history, scrollback)
    match key {
        "UpArrow" | "DownArrow" | "PageUp" | "PageDown" => return pty(key),
        _ => {}
    }

    // 6. Ctrl+C -> PTY (interrupt)
    if mod_mask & MOD_CTRL != 0 && matches!(key, "c" | "C") {
        return pty("c");
    }

    // 7. Tab -> PTY (shell completion)
    if key == "Tab" {
        return pty("Tab");
    }

    // 8. Everything else -> Reject (TextEdit handles locally, composer sync mirrors to PTY).
    //    Includes: printable chars, Backspace, Delete, LeftArrow, RightArrow, Home, End.
    PromptKeyAction::Reject
}

fn pty(s: &str) -> PromptKeyAction {
    PromptKeyAction::PtyKey(s.to_string())
}

fn is_enter_key(key: &str) -> bool {
    matches!(key, "\n" | "\r" | "Return")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_encoding;

    fn m(ctrl: bool, shift: bool, alt: bool, meta: bool) -> u32 {
        key_encoding::mod_bits(ctrl, shift, alt, meta)
    }

    #[test]
    fn ctrl_space_opens_file_picker() {
        assert_eq!(
            route_prompt_key(m(true, false, false, false), " ", false),
            PromptKeyAction::OpenFilePicker
        );
        assert_eq!(
            route_prompt_key(m(false, false, false, false), "\0", false),
            PromptKeyAction::OpenFilePicker
        );
        // Plain space -> Reject (TextEdit inserts it)
        assert_eq!(
            route_prompt_key(m(false, false, false, false), " ", false),
            PromptKeyAction::Reject
        );
    }

    #[test]
    fn enter_submits() {
        assert_eq!(
            route_prompt_key(m(false, false, false, false), "Return", false),
            PromptKeyAction::Submit
        );
        assert_eq!(
            route_prompt_key(m(false, true, false, false), "Return", true),
            PromptKeyAction::Submit
        );
    }

    #[test]
    fn escape_sends_to_pty() {
        let no_mod = m(false, false, false, false);
        let a = route_prompt_key(no_mod, "Escape", false);
        assert!(matches!(a, PromptKeyAction::PtyKey(ref s) if s == "Escape"));
    }

    #[test]
    fn vertical_arrows_send_to_pty() {
        let no_mod = m(false, false, false, false);
        assert!(matches!(
            route_prompt_key(no_mod, "UpArrow", false),
            PromptKeyAction::PtyKey(ref s) if s == "UpArrow"
        ));
        assert!(matches!(
            route_prompt_key(no_mod, "DownArrow", false),
            PromptKeyAction::PtyKey(ref s) if s == "DownArrow"
        ));
    }

    #[test]
    fn horizontal_arrows_rejected_to_textedit() {
        let no_mod = m(false, false, false, false);
        assert_eq!(
            route_prompt_key(no_mod, "LeftArrow", false),
            PromptKeyAction::Reject
        );
        assert_eq!(
            route_prompt_key(no_mod, "RightArrow", false),
            PromptKeyAction::Reject
        );
    }

    #[test]
    fn backspace_delete_rejected_to_textedit() {
        let no_mod = m(false, false, false, false);
        assert_eq!(
            route_prompt_key(no_mod, "Backspace", false),
            PromptKeyAction::Reject
        );
        assert_eq!(
            route_prompt_key(no_mod, "Delete", false),
            PromptKeyAction::Reject
        );
    }

    #[test]
    fn printable_chars_rejected_to_textedit() {
        let no_mod = m(false, false, false, false);
        assert_eq!(
            route_prompt_key(no_mod, "a", false),
            PromptKeyAction::Reject
        );
        assert_eq!(
            route_prompt_key(no_mod, "z", false),
            PromptKeyAction::Reject
        );
    }

    #[test]
    fn tab_sends_to_pty() {
        let no_mod = m(false, false, false, false);
        assert!(matches!(
            route_prompt_key(no_mod, "Tab", false),
            PromptKeyAction::PtyKey(ref s) if s == "Tab"
        ));
    }

    #[test]
    fn ctrl_c_sends_to_pty() {
        assert!(matches!(
            route_prompt_key(m(true, false, false, false), "c", false),
            PromptKeyAction::PtyKey(ref s) if s == "c"
        ));
    }
}
