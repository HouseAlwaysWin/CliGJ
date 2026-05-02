use super::timers_terminal::*;
use cligj_terminal::render::{ColoredLine, ColoredSpan};
use cligj_terminal::types::{RawPtyEvent, RawPtyMode};

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

#[test]
fn interactive_prompt_from_cursor_line_strips_prompt_marker() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut tab = crate::gui::state::TabState::new(1, tx, None);
    tab.terminal_mode = crate::gui::state::TerminalMode::InteractiveAi;
    tab.terminal_lines = vec![line("\u{203a} /skills")];
    tab.terminal_cursor_row = Some(0);
    tab.terminal_cursor_col = Some(9);

    let prompt = interactive_prompt_from_cursor_line(&tab);
    assert_eq!(prompt.as_deref(), Some("/skills"));
}

#[test]
fn interactive_prompt_from_cursor_line_ignores_empty_prompt() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut tab = crate::gui::state::TabState::new(1, tx, None);
    tab.terminal_mode = crate::gui::state::TerminalMode::InteractiveAi;
    tab.terminal_lines = vec![line("\u{203a} ")];
    tab.terminal_cursor_row = Some(0);
    tab.terminal_cursor_col = Some(2);

    assert_eq!(interactive_prompt_from_cursor_line(&tab), None);
}

#[test]
fn interactive_prompt_from_cursor_line_ignores_status_line_without_prompt_marker() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut tab = crate::gui::state::TabState::new(1, tx, None);
    tab.terminal_mode = crate::gui::state::TerminalMode::InteractiveAi;
    tab.terminal_lines = vec![line("gpt-5.4 xhigh · D:\\Projects\\CliGJ")];
    tab.terminal_cursor_row = Some(0);
    tab.terminal_cursor_col = Some(10);

    assert_eq!(interactive_prompt_from_cursor_line(&tab), None);
}

#[test]
fn interactive_prompt_from_cursor_line_ignores_banner_line_without_prompt_space() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut tab = crate::gui::state::TabState::new(1, tx, None);
    tab.terminal_mode = crate::gui::state::TerminalMode::InteractiveAi;
    tab.terminal_lines = vec![line(">_ OpenAI Codex (v0.128.0)")];
    tab.terminal_cursor_row = Some(0);
    tab.terminal_cursor_col = Some(3);

    assert_eq!(interactive_prompt_from_cursor_line(&tab), None);
}

#[test]
fn interactive_prompt_from_visible_footer_falls_back_when_cursor_is_elsewhere() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut tab = crate::gui::state::TabState::new(1, tx, None);
    tab.terminal_mode = crate::gui::state::TerminalMode::InteractiveAi;
    tab.terminal_lines = vec![
        line("Shift+Tab to accept edits"),
        line("> /skills"),
        line("skills  list, enable, disable"),
    ];
    tab.terminal_cursor_row = Some(2);
    tab.terminal_cursor_col = Some(6);

    let prompt = interactive_prompt_from_visible_footer(&tab);
    assert_eq!(prompt.as_deref(), Some("/skills"));
}

#[test]
fn interactive_prompt_from_visible_footer_ignores_plain_slash_output() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut tab = crate::gui::state::TabState::new(1, tx, None);
    tab.terminal_mode = crate::gui::state::TerminalMode::InteractiveAi;
    tab.terminal_lines = vec![
        line("Tip: Try /model to change model"),
        line("/memories configure memory use and generation"),
        line("Use Enter to submit"),
    ];

    assert_eq!(interactive_prompt_from_visible_footer(&tab), None);
}

#[test]
fn interactive_prompt_sync_does_not_repopulate_cleared_ui_prompt() {
    assert!(!should_sync_interactive_prompt(
        "",
        "",
        "Run /review on my current changes"
    ));
}

#[test]
fn interactive_prompt_sync_allows_terminal_to_extend_existing_ui_prompt() {
    assert!(should_sync_interactive_prompt("/rev", "/rev", "/review"));
}

#[test]
fn interactive_prompt_sync_ignores_non_command_text() {
    assert!(!should_sync_interactive_prompt(
        "Run /review",
        "Run /review",
        "Run /review on my current changes"
    ));
}

#[test]
fn interactive_history_without_overlap_preserves_existing_lines() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut tab = crate::gui::state::TabState::new(1, tx, None);
    tab.terminal_mode = crate::gui::state::TerminalMode::InteractiveAi;
    tab.interactive_history_lines = vec![line("older reply"), line("older footer")];

    append_interactive_history_block(
        &mut tab,
        &[
            line("new snapshot line 1"),
            line("new snapshot line 2"),
            line("new snapshot line 3"),
            line("new snapshot line 4"),
        ],
    );

    let text: Vec<String> = tab
        .interactive_history_lines
        .iter()
        .map(line_plain_text)
        .collect();
    assert_eq!(
        text,
        vec![
            "older reply".to_string(),
            "older footer".to_string(),
            "new snapshot line 1".to_string(),
            "new snapshot line 2".to_string(),
            "new snapshot line 3".to_string(),
            "new snapshot line 4".to_string(),
        ]
    );
}

#[test]
fn interactive_reset_buffer_keeps_archived_history() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut tab = crate::gui::state::TabState::new(1, tx, None);
    tab.terminal_mode = crate::gui::state::TerminalMode::InteractiveAi;
    tab.interactive_history_lines = vec![line("codex reply kept"), line("follow-up kept")];
    tab.interactive_frame_lines = vec![line("old frame")];
    tab.terminal_lines = vec![
        line("codex reply kept"),
        line("follow-up kept"),
        line("old frame"),
    ];

    super::timers_terminal_interactive::apply_interactive_replace(
        &mut tab,
        &[],
        true,
        Some(cligj_terminal::types::ResetReason::ClearScreen),
        None,
        None,
        8,
        10,
        vec![line("new frame top"), line("new frame bottom")],
    );

    let archived: Vec<String> = tab
        .interactive_history_lines
        .iter()
        .map(line_plain_text)
        .collect();
    assert_eq!(
        archived,
        vec![
            "codex reply kept".to_string(),
            "follow-up kept".to_string(),
            "old frame".to_string(),
        ]
    );
    let rendered: Vec<String> = tab.terminal_lines.iter().map(line_plain_text).collect();
    assert_eq!(
        rendered,
        vec![
            "codex reply kept".to_string(),
            "follow-up kept".to_string(),
            "old frame".to_string(),
            "new frame top".to_string(),
            "new frame bottom".to_string(),
        ]
    );
}

#[test]
fn interactive_reset_archives_previous_visible_frame_before_replacing_it() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut tab = crate::gui::state::TabState::new(1, tx, None);
    tab.terminal_mode = crate::gui::state::TerminalMode::InteractiveAi;
    tab.interactive_frame_lines = vec![
        line("assistant reply line 1"),
        line("assistant reply line 2"),
        line("? for shortcuts"),
    ];
    tab.terminal_lines = tab.interactive_frame_lines.clone();

    super::timers_terminal_interactive::apply_interactive_replace(
        &mut tab,
        &[],
        true,
        Some(cligj_terminal::types::ResetReason::ClearScreen),
        None,
        None,
        12,
        14,
        vec![line("fresh prompt"), line("waiting input")],
    );

    let archived: Vec<String> = tab
        .interactive_history_lines
        .iter()
        .map(line_plain_text)
        .collect();
    assert_eq!(
        archived,
        vec![
            "assistant reply line 1".to_string(),
            "assistant reply line 2".to_string(),
            "? for shortcuts".to_string(),
        ]
    );
}

#[test]
fn interactive_resize_reset_does_not_archive_previous_frame_again() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut tab = crate::gui::state::TabState::new(1, tx, None);
    tab.terminal_mode = crate::gui::state::TerminalMode::InteractiveAi;
    tab.interactive_history_lines = vec![line("older archived reply")];
    tab.interactive_frame_lines = vec![
        line("pre-resize frame line 1"),
        line("pre-resize frame line 2"),
    ];
    tab.terminal_lines = vec![
        line("older archived reply"),
        line("pre-resize frame line 1"),
        line("pre-resize frame line 2"),
    ];

    super::timers_terminal_interactive::apply_interactive_replace(
        &mut tab,
        &[],
        true,
        Some(cligj_terminal::types::ResetReason::Resize),
        None,
        None,
        20,
        22,
        vec![
            line("post-resize frame line 1"),
            line("post-resize frame line 2"),
        ],
    );

    let archived: Vec<String> = tab
        .interactive_history_lines
        .iter()
        .map(line_plain_text)
        .collect();
    assert_eq!(archived, vec!["older archived reply".to_string()]);
}

#[test]
fn interactive_terminal_history_prefers_raw_replay_over_live_snapshot() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut tab = crate::gui::state::TabState::new(1, tx, None);
    tab.terminal_mode = crate::gui::state::TerminalMode::InteractiveAi;
    tab.terminal_lines = vec![line("only current snapshot")];
    tab.raw_pty_events = vec![
        RawPtyEvent::RenderMode {
            mode: RawPtyMode::InteractiveAi,
        },
        RawPtyEvent::Resize { cols: 24, rows: 6 },
        RawPtyEvent::Bytes(b"older line 1\r\nolder line 2\r\ncurrent line\r\n".to_vec()),
    ];

    let text = crate::gui::run::helpers::terminal_history_plain_text(&tab);
    assert!(text.contains("older line 1"));
    assert!(text.contains("older line 2"));
    assert!(!text.contains("only current snapshot"));
}
