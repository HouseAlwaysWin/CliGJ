use slint::{ComponentHandle, SharedString};

use cligj_core::config::AppConfig;

use super::slint_ui::{AppTheme, AppWindow, TerminalHistoryWindow};
use super::state::{GuiState, TerminalMode};
use super::ui_sync::{
    clamp_saved_scroll_top, push_terminal_view_to_ui, terminal_scroll_top_for_tab,
};

pub(crate) const DEFAULT_UI_ZOOM_PERCENT: i32 = 100;
pub(crate) const MIN_UI_ZOOM_PERCENT: i32 = 70;
pub(crate) const MAX_UI_ZOOM_PERCENT: i32 = 150;
pub(crate) const UI_ZOOM_STEP_PERCENT: i32 = 10;
pub(crate) const DEFAULT_TERMINAL_ROW_HEIGHT_PX: f32 = 18.0;

pub(crate) fn clamp_ui_zoom_percent(percent: i32) -> i32 {
    percent.clamp(MIN_UI_ZOOM_PERCENT, MAX_UI_ZOOM_PERCENT)
}

pub(crate) fn normalize_ui_zoom_percent(percent: i32) -> i32 {
    clamp_ui_zoom_percent(percent)
}

pub(crate) fn parse_ui_zoom_percent(text: &str) -> Option<i32> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .trim_end_matches('%')
        .trim()
        .parse::<i32>()
        .ok()
        .map(clamp_ui_zoom_percent)
}

pub(crate) fn format_ui_zoom_percent(percent: i32) -> String {
    format!("{}%", clamp_ui_zoom_percent(percent))
}

pub(crate) fn ui_zoom_factor(percent: i32) -> f32 {
    clamp_ui_zoom_percent(percent) as f32 / 100.0
}

fn persist_ui_zoom_percent(percent: i32) -> Result<(), String> {
    let mut cfg = AppConfig::load_or_default().map_err(|e| format!("load config: {e}"))?;
    cfg.set_ui_zoom_percent(percent);
    cfg.save().map_err(|e| format!("save config: {e}"))
}

fn apply_ui_zoom_globals(
    ui: &AppWindow,
    history_window: Option<&TerminalHistoryWindow>,
    factor: f32,
) {
    ui.global::<AppTheme>().set_ui_zoom_factor(factor);
    if let Some(history_window) = history_window {
        history_window
            .global::<AppTheme>()
            .set_ui_zoom_factor(factor);
    }
}

fn refresh_current_terminal_after_zoom(ui: &AppWindow, s: &mut GuiState) {
    if s.current >= s.tabs.len() {
        return;
    }
    let current = s.current;
    let tab = &mut s.tabs[current];
    let vh = ui.get_ws_terminal_viewport_height_px().max(1.0);
    let scroll =
        if tab.terminal_mode == TerminalMode::InteractiveAi && tab.interactive_follow_output {
            terminal_scroll_top_for_tab(tab, vh)
        } else if tab.auto_scroll {
            terminal_scroll_top_for_tab(tab, vh)
        } else {
            clamp_saved_scroll_top(tab, vh)
        };
    ui.invoke_ws_apply_terminal_scroll_top_px(scroll);
    push_terminal_view_to_ui(ui, tab, Some(scroll));
    tab.terminal_saved_scroll_top_px = scroll;
}

fn retune_terminal_metrics(ui: &AppWindow, s: &mut GuiState, factor: f32) {
    let new_row_height = (DEFAULT_TERMINAL_ROW_HEIGHT_PX * factor).max(1.0);
    for tab in &mut s.tabs {
        let old_row_height = tab.terminal_row_height_px.max(1.0);
        let ratio = new_row_height / old_row_height;
        tab.terminal_row_height_px = new_row_height;
        tab.terminal_saved_scroll_top_px *= ratio;
        tab.terminal_scroll_top_px *= ratio;
        if tab.last_pushed_scroll_top >= 0.0 {
            tab.last_pushed_scroll_top *= ratio;
        }
    }
    ui.invoke_ws_bump_terminal_size();
    refresh_current_terminal_after_zoom(ui, s);
}

pub(crate) fn apply_ui_zoom_percent(
    ui: &AppWindow,
    history_window: Option<&TerminalHistoryWindow>,
    s: &mut GuiState,
    percent: i32,
    persist: bool,
) -> Result<i32, String> {
    let percent = clamp_ui_zoom_percent(percent);
    let factor = ui_zoom_factor(percent);
    apply_ui_zoom_globals(ui, history_window, factor);
    s.startup_ui_zoom_percent = percent;
    let label = format_ui_zoom_percent(percent);
    ui.set_ws_shell_startup_ui_zoom(SharedString::from(label.as_str()));
    retune_terminal_metrics(ui, s, factor);
    if persist {
        persist_ui_zoom_percent(percent)?;
    }
    Ok(percent)
}

pub(crate) fn adjust_ui_zoom_percent(
    ui: &AppWindow,
    history_window: Option<&TerminalHistoryWindow>,
    s: &mut GuiState,
    delta_percent: i32,
    persist: bool,
) -> Result<i32, String> {
    apply_ui_zoom_percent(
        ui,
        history_window,
        s,
        s.startup_ui_zoom_percent + delta_percent,
        persist,
    )
}

pub(crate) fn reset_ui_zoom_percent(
    ui: &AppWindow,
    history_window: Option<&TerminalHistoryWindow>,
    s: &mut GuiState,
    persist: bool,
) -> Result<i32, String> {
    apply_ui_zoom_percent(ui, history_window, s, DEFAULT_UI_ZOOM_PERCENT, persist)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format_zoom_percent() {
        assert_eq!(parse_ui_zoom_percent("125%"), Some(125));
        assert_eq!(parse_ui_zoom_percent(" 90 "), Some(90));
        assert_eq!(parse_ui_zoom_percent(""), None);
        assert_eq!(format_ui_zoom_percent(105), "105%");
    }

    #[test]
    fn clamp_zoom_percent_to_supported_range() {
        assert_eq!(clamp_ui_zoom_percent(10), MIN_UI_ZOOM_PERCENT);
        assert_eq!(clamp_ui_zoom_percent(500), MAX_UI_ZOOM_PERCENT);
        assert_eq!(clamp_ui_zoom_percent(100), 100);
    }
}
