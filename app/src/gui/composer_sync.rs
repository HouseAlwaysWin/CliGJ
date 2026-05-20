//! Composer line -> ConPTY sync while keeping the UI composer authoritative.

use cligj_terminal::render::ColoredLine;

use super::slint_ui::AppWindow;
use super::state::{GuiState, TabState, TerminalMode};

const NATIVE_RESYNC_CONFIRM_TICKS: u8 = 2;
const FULL_REWRITE_BACKSPACE_HEADROOM: usize = 32;
const INTERACTIVE_PROMPT_TAIL_SCAN_LINES: usize = 12;

pub(crate) fn diff_composer_to_conpty(prev: &str, cur: &str) -> Vec<u8> {
    // Keep deletion semantics aligned with keyboard Backspace encoding (`0x7f` in key_encoding).
    // Mixing `0x08` here with `0x7f` from key events can desync line editors in some TTY apps.
    const PTY_BACKSPACE: u8 = 0x7f;
    if prev == cur {
        return Vec::new();
    }
    let pa: Vec<char> = prev.chars().collect();
    let ca: Vec<char> = cur.chars().collect();
    let mut i = 0usize;
    while i < pa.len() && i < ca.len() && pa[i] == ca[i] {
        i += 1;
    }
    let mut out = Vec::new();
    for _ in 0..pa.len().saturating_sub(i) {
        out.push(PTY_BACKSPACE);
    }
    for c in ca.iter().skip(i) {
        let mut buf = [0u8; 4];
        let t = c.encode_utf8(&mut buf);
        out.extend_from_slice(t.as_bytes());
    }
    out
}

fn line_plain_text(line: &ColoredLine) -> String {
    let mut text = String::new();
    for span in &line.spans {
        text.push_str(span.text.as_str());
    }
    text
}

fn strip_interactive_prompt_prefix(text: &str) -> Option<&str> {
    let trimmed = text.trim_start();
    let Some(first) = trimmed.chars().next() else {
        return None;
    };
    if first == '\u{203a}' {
        let rest = &trimmed[first.len_utf8()..];
        return Some(rest.strip_prefix(' ').unwrap_or(rest));
    }
    if let Some(rest) = trimmed.strip_prefix("> ") {
        return Some(rest);
    }
    None
}

fn extract_interactive_prompt_from_line(line: &ColoredLine) -> Option<String> {
    let line_text = line_plain_text(line);
    let prompt = strip_interactive_prompt_prefix(line_text.as_str())?.trim_end();
    if prompt.is_empty() {
        return None;
    }
    Some(prompt.to_string())
}

fn is_interactive_prompt_placeholder(prompt: &str) -> bool {
    let lowered = prompt.trim().to_ascii_lowercase();
    lowered.contains("type your message")
        || lowered.contains("type a message")
        || lowered.contains("@path/to/file")
}

fn push_native_prompt_candidate(
    out: &mut Vec<String>,
    candidate: Option<String>,
    ui_prompt: &str,
    prev_estimate: &str,
) {
    let Some(candidate) = candidate else {
        return;
    };
    if is_interactive_prompt_placeholder(candidate.as_str()) || out.contains(&candidate) {
        return;
    }
    if !ui_prompt.is_empty() && candidate == ui_prompt {
        out.insert(0, candidate);
        return;
    }
    if !prev_estimate.is_empty() && candidate == prev_estimate {
        out.insert(0, candidate);
        return;
    }
    if !ui_prompt.is_empty() && candidate.starts_with(ui_prompt) {
        out.insert(out.len().min(1), candidate);
        return;
    }
    if !prev_estimate.is_empty() && candidate.starts_with(prev_estimate) {
        out.insert(out.len().min(2), candidate);
        return;
    }
    out.push(candidate);
}

fn active_interactive_native_prompt(
    tab: &TabState,
    ui_prompt: &str,
    prev_estimate: &str,
) -> Option<String> {
    if tab.terminal_mode != TerminalMode::InteractiveAi {
        return None;
    }
    let mut candidates = Vec::new();

    if let Some(row) = tab.terminal_cursor_row {
        push_native_prompt_candidate(
            &mut candidates,
            tab.terminal_lines
                .get(row)
                .and_then(extract_interactive_prompt_from_line),
            ui_prompt,
            prev_estimate,
        );
    }

    let tail_start = tab
        .terminal_lines
        .len()
        .saturating_sub(INTERACTIVE_PROMPT_TAIL_SCAN_LINES);
    for line in tab.terminal_lines[tail_start..].iter().rev() {
        push_native_prompt_candidate(
            &mut candidates,
            extract_interactive_prompt_from_line(line),
            ui_prompt,
            prev_estimate,
        );
    }

    candidates.into_iter().next()
}

fn full_rewrite_composer_to_conpty(
    prev_estimate: &str,
    native_prompt: Option<&str>,
    cur: &str,
) -> Vec<u8> {
    const PTY_BACKSPACE: u8 = 0x7f;
    let clear_chars = prev_estimate
        .chars()
        .count()
        .max(native_prompt.map(|text| text.chars().count()).unwrap_or(0))
        .max(cur.chars().count())
        .saturating_add(FULL_REWRITE_BACKSPACE_HEADROOM);
    let mut out = Vec::new();
    out.resize(clear_chars, PTY_BACKSPACE);
    for c in cur.chars() {
        let mut buf = [0u8; 4];
        let t = c.encode_utf8(&mut buf);
        out.extend_from_slice(t.as_bytes());
    }
    out
}

fn reset_native_resync_candidate(tab: &mut TabState) {
    tab.composer_native_resync_candidate.clear();
    tab.composer_native_resync_candidate_ticks = 0;
}

fn observe_native_resync_candidate(
    candidate: &str,
    ticks: u8,
    native_prompt: &str,
) -> (String, u8, bool) {
    let next_ticks = if candidate == native_prompt {
        ticks.saturating_add(1)
    } else {
        1
    };
    (
        native_prompt.to_string(),
        next_ticks,
        next_ticks >= NATIVE_RESYNC_CONFIRM_TICKS,
    )
}

pub(crate) fn sync_composer_line_to_conpty(ui: &AppWindow, s: &mut GuiState) {
    #[cfg(not(target_os = "windows"))]
    let _ = (ui, s);

    #[cfg(target_os = "windows")]
    {
        use std::io::Write;

        if s.current >= s.tabs.len() {
            return;
        }
        let tab = &mut s.tabs[s.current];

        // Only mirror prompt here - avoid `tab_update_from_ui` (it syncs full tab state every tick).
        tab.prompt = ui.get_ws_prompt();
        let cur = tab.prompt.to_string();
        let prev = tab.composer_pty_mirror.clone();
        let native_prompt = active_interactive_native_prompt(tab, cur.as_str(), prev.as_str());

        // If the active native prompt already matches the UI, repair the mirror baseline instead
        // of sending more edits from stale state.
        if native_prompt.as_deref() == Some(cur.as_str()) {
            tab.composer_pty_mirror = cur;
            reset_native_resync_candidate(tab);
            return;
        }

        let bytes = if cur == prev {
            let Some(native_prompt) = native_prompt.as_deref() else {
                reset_native_resync_candidate(tab);
                return;
            };
            let (candidate, ticks, confirmed) = observe_native_resync_candidate(
                tab.composer_native_resync_candidate.as_str(),
                tab.composer_native_resync_candidate_ticks,
                native_prompt,
            );
            tab.composer_native_resync_candidate = candidate;
            tab.composer_native_resync_candidate_ticks = ticks;
            if !confirmed {
                return;
            }
            reset_native_resync_candidate(tab);
            full_rewrite_composer_to_conpty(prev.as_str(), Some(native_prompt), &cur)
        } else {
            reset_native_resync_candidate(tab);
            if cur.starts_with(prev.as_str()) {
                diff_composer_to_conpty(prev.as_str(), &cur)
            } else {
                full_rewrite_composer_to_conpty(prev.as_str(), native_prompt.as_deref(), &cur)
            }
        };
        if bytes.is_empty() {
            return;
        }

        let Some(writer) = tab.pty_writer.as_mut() else {
            return;
        };
        let _ = writer.write_all(&bytes);
        let _ = writer.flush();
        tab.composer_pty_mirror = cur;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        active_interactive_native_prompt, diff_composer_to_conpty,
        full_rewrite_composer_to_conpty, observe_native_resync_candidate,
    };
    use crate::gui::state::{TabState, TerminalMode};
    use cligj_terminal::render::{ColoredLine, ColoredSpan};

    fn line(text: &str) -> ColoredLine {
        ColoredLine {
            blank: text.trim().is_empty(),
            spans: if text.is_empty() {
                Vec::new()
            } else {
                vec![ColoredSpan {
                    text: text.to_string(),
                    fg: [240, 240, 240],
                    bg: [18, 18, 18],
                }]
            },
        }
    }

    #[test]
    fn append_from_empty() {
        assert_eq!(diff_composer_to_conpty("", "ab"), b"ab");
    }

    #[test]
    fn shrink_one_char() {
        assert_eq!(diff_composer_to_conpty("ab", "a"), vec![0x7f]);
    }

    #[test]
    fn common_prefix_replace_tail() {
        let d = diff_composer_to_conpty("hello@x", "hello@yz");
        assert!(d.iter().any(|&b| b == b'y' || b == b'z'));
    }

    #[test]
    fn clear_all() {
        assert_eq!(
            diff_composer_to_conpty("abc", "").as_slice(),
            &[0x7f, 0x7f, 0x7f]
        );
    }

    #[test]
    fn full_rewrite_clears_longest_estimate_then_retypes_ui() {
        let bytes = full_rewrite_composer_to_conpty("ab", Some("abcd"), "ax");
        assert_eq!(bytes.len(), 4 + 32 + 2);
        assert!(bytes[..36].iter().all(|&b| b == 0x7f));
        assert_eq!(&bytes[36..], b"ax");
    }

    #[test]
    fn native_resync_candidate_requires_confirmation() {
        let (_, ticks, confirmed) = observe_native_resync_candidate("", 0, "/review");
        assert_eq!(ticks, 1);
        assert!(!confirmed);

        let (candidate, ticks, confirmed) =
            observe_native_resync_candidate("/review", 1, "/review");
        assert_eq!(candidate, "/review");
        assert_eq!(ticks, 2);
        assert!(confirmed);
    }

    #[test]
    fn native_resync_candidate_resets_for_new_prompt_text() {
        let (candidate, ticks, confirmed) =
            observe_native_resync_candidate("/review", 1, "/status");
        assert_eq!(candidate, "/status");
        assert_eq!(ticks, 1);
        assert!(!confirmed);
    }

    #[test]
    fn active_native_prompt_prefers_matching_tail_prompt_over_placeholder() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_lines = vec![
            line("> Implement {feature}"),
            line("model   gpt-5.4 xhigh"),
            line("> Type your message or @path/to/file"),
            line("> /stats"),
        ];

        let prompt = active_interactive_native_prompt(&tab, "/stats", "/sta");
        assert_eq!(prompt.as_deref(), Some("/stats"));
    }

    #[test]
    fn active_native_prompt_uses_cursor_line_for_plain_text_edits() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_cursor_row = Some(1);
        tab.terminal_lines = vec![line("history"), line("> st")];

        let prompt = active_interactive_native_prompt(&tab, "st", "s");
        assert_eq!(prompt.as_deref(), Some("st"));
    }
}
