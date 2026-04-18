//! Composer / TTY prompt key routing (formerly `gj_prompt_drop_zone` `key-pressed` rules).

use super::key_encoding::{normalize_tty_key_token, MOD_ALT, MOD_CTRL};

/// What to do for one `TextEdit` `key-pressed` event (`accept` vs `reject` decided in Slint).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKeyAction {
    /// Let `TextEdit` handle the key (insert character, etc.).
    Reject,
    ToggleRawInput,
    Submit,
    HistoryPrev,
    HistoryNext,
    /// Encode with `key_encoding::encode_for_pty(mod_mask, …)` then write to ConPTY.
    PtyKey(String),
}

#[must_use]
pub fn route_prompt_key(raw_tty: bool, mod_mask: u32, key: &str, shift: bool) -> PromptKeyAction {
    let key = normalize_tty_key_token(key);
    
    // 1. 特殊功能鍵優先
    if mod_mask & MOD_CTRL != 0 && matches!(key, "r" | "R") {
        return PromptKeyAction::ToggleRawInput;
    }

    // 2. 統一 Enter：不論 Raw 模式與否，一律觸發智慧提交邏輯
    if is_enter_key(key) {
        return PromptKeyAction::Submit;
    }

    // 3. Alt 組合鍵 (通常發送 PTY 序列)
    if mod_mask & MOD_ALT != 0 {
        match key {
            "UpArrow" => return pty("UpArrow"),
            "DownArrow" => return pty("DownArrow"),
            "RightArrow" => return pty("RightArrow"),
            "LeftArrow" => return pty("LeftArrow"),
            _ => {}
        }
    }

    // 4. 非 Raw 模式 (Composer)：方向鍵處理歷史紀錄
    if !raw_tty {
        if key == "UpArrow" {
            return PromptKeyAction::HistoryPrev;
        }
        if key == "DownArrow" {
            return PromptKeyAction::HistoryNext;
        }
    }

    // 5. Raw 模式 (R ON)：發送特殊按鍵序列給 PTY
    if raw_tty {
        match key {
            "UpArrow" | "DownArrow" | "RightArrow" | "LeftArrow" | "Home" | "End" | "PageUp"
            | "PageDown" | "Delete" | "Escape" | "Backspace" => return pty(key),
            _ => {}
        }
    }

    // 6. 其他常用組合
    if mod_mask & MOD_CTRL != 0 && matches!(key, "c" | "C") {
        return pty("c");
    }
    if key == "Tab" {
        return pty("Tab");
    }

    // 7. 若在 Raw 模式下仍未處理，直接透傳給 PTY
    if raw_tty {
        return pty(key);
    }

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
    use crate::terminal::key_encoding;

    fn m(ctrl: bool, shift: bool, alt: bool, meta: bool) -> u32 {
        key_encoding::mod_bits(ctrl, shift, alt, meta)
    }

    #[test]
    fn ctrl_r_toggles_raw() {
        assert_eq!(
            route_prompt_key(false, m(true, false, false, false), "r", false),
            PromptKeyAction::ToggleRawInput
        );
    }

    #[test]
    fn composer_enter_submits() {
        assert_eq!(
            route_prompt_key(false, m(false, false, false, false), "Return", false),
            PromptKeyAction::Submit
        );
        // Unified Enter: even with shift, it should submit
        assert_eq!(
            route_prompt_key(false, m(false, true, false, false), "Return", true),
            PromptKeyAction::Submit
        );
    }

    #[test]
    fn composer_arrows_are_history() {
        assert_eq!(
            route_prompt_key(false, m(false, false, false, false), "UpArrow", false),
            PromptKeyAction::HistoryPrev
        );
    }

    #[test]
    fn raw_sends_named_keys() {
        let a = route_prompt_key(true, m(false, false, false, false), "LeftArrow", false);
        assert!(matches!(a, PromptKeyAction::PtyKey(ref s) if s == "LeftArrow"));
    }
}
