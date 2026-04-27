use super::timers_terminal::*;
use cligj_terminal::render::{ColoredLine, ColoredSpan};

fn line(text: &str) -> ColoredLine {
    ColoredLine {
        blank: false,
        spans: vec![ColoredSpan {
            text: text.to_string(),
            fg: [240, 240, 240],
            bg: [18, 18, 18],
        }],
    }
}

#[test]
fn codex_snapshot_with_shell_preamble_is_trimmed_not_dropped() {
    let mut lines = vec![
        line("Microsoft Windows [Version 10.0.19045]"),
        line("(c) Microsoft Corporation."),
        line("D:\\Projects\\CliGJ>codex"),
        line(">_ OpenAI Codex (v0.123.0)"),
        line("model: gpt-5.4 xhigh /model to change"),
    ];
    let markers = vec!["openai codex".to_string(), "/model to change".to_string()];

    assert!(!trim_or_drop_shell_preamble_snapshot(&mut lines, &markers));
    assert_eq!(line_plain_text(&lines[0]), ">_ OpenAI Codex (v0.123.0)");
}

#[test]
fn repainted_frame_transition_without_scroll_does_not_archive() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut tab = crate::gui::state::TabState::new(1, tx, None);
    tab.terminal_mode = crate::gui::state::TerminalMode::InteractiveAi;
    tab.interactive_launcher_program = "codex".to_string();
    tab.interactive_archive_repainted_frames = true;
    tab.interactive_frame_lines = vec![
        line("╭────────────────────────╮"),
        line("│  >_ OpenAI Codex       │"),
        line("│  Welcome               │"),
        line("\u{203a} Use /skills to list available skills"),
    ];
    let status = vec![
        line("/status"),
        line("╭────────────────────────╮"),
        line("│  >_ OpenAI Codex       │"),
        line("│  Model: gpt-5.4        │"),
        line("\u{203a} Use /skills to list available skills"),
    ];

    maybe_archive_repainted_frame_before_replace(&mut tab, &status);
    assert!(tab.interactive_history_lines.is_empty());
}

#[test]
fn repainted_footer_typing_does_not_archive_each_prompt_edit() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut tab = crate::gui::state::TabState::new(1, tx, None);
    tab.terminal_mode = crate::gui::state::TerminalMode::InteractiveAi;
    tab.interactive_launcher_program = "codex".to_string();
    tab.interactive_archive_repainted_frames = true;
    tab.interactive_frame_lines = vec![line("│  >_ OpenAI Codex       │"), line("\u{203a} /st")];
    let next = vec![line("│  >_ OpenAI Codex       │"), line("\u{203a} /sta")];

    maybe_archive_repainted_frame_before_replace(&mut tab, &next);
    assert!(tab.interactive_history_lines.is_empty());
}
