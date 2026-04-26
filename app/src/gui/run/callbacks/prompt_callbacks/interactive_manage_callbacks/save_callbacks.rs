use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use slint::{ComponentHandle, Model, SharedString};

use crate::gui::interactive_commands::{
    pinned_footer_lines_for_specs, spec_for_program_in_specs,
    sync_interactive_command_choices_to_ui,
};
use crate::gui::slint_ui::AppWindow;
use crate::gui::state::{GuiState, TerminalMode};
use cligj_core::config::{AppConfig, InteractiveCommandConfig};

use super::super::super::refresh_terminal_tab_view;

pub(super) fn connect(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_save = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_save_interactive_manage(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let rows_m = ui.get_ws_interactive_manage_rows();
        let n = rows_m.row_count();
        let mut seen = HashSet::<String>::new();
        let existing_specs = st_save.borrow().interactive_commands.clone();
        let mut out: Vec<InteractiveCommandConfig> = Vec::new();
        for i in 0..n {
            let row = rows_m.row_data(i).unwrap();
            let name = row.name.to_string();
            let line = row.line.to_string();
            let pinned_footer_lines = row.pinned_footer_lines.to_string();
            let markers = row.markers.to_string();
            let nt = name.trim();
            let lt = line.trim();
            if nt.is_empty() && lt.is_empty() {
                continue;
            }
            if nt.is_empty() {
                eprintln!("CliGJ: interactive command row needs a display name");
                return;
            }
            if lt.is_empty() {
                eprintln!("CliGJ: interactive command row needs a command line");
                return;
            }
            let nt = nt.to_string();
            if !seen.insert(nt.clone()) {
                eprintln!("CliGJ: duplicate interactive command name: {nt}");
                return;
            }
            let pinned = if pinned_footer_lines.trim().is_empty() {
                0
            } else {
                match pinned_footer_lines.trim().parse::<usize>() {
                    Ok(v) => v,
                    Err(_) => {
                        eprintln!(
                            "CliGJ: interactive command pinned footer rows must be a non-negative integer"
                        );
                        return;
                    }
                }
            };
            let mut spec = existing_specs
                .iter()
                .find(|spec| spec.name == nt || spec.command == lt)
                .cloned()
                .unwrap_or_else(|| {
                    InteractiveCommandConfig::with_defaults(nt.clone(), lt.to_string(), pinned)
                });
            spec.name = nt;
            spec.command = lt.to_string();
            spec.interactive_cli = row.interactive_cli;
            spec.pinned_footer_lines = pinned;
            spec.markers = parse_marker_editor_text(markers.as_str());
            spec.archive_repainted_frames = row.archive_repainted_frames;
            out.push(spec);
        }
        if out.is_empty() {
            eprintln!("CliGJ: need at least one interactive command");
            return;
        }
        let specs = out.clone();
        let refresh_current_interactive = {
            let mut s = st_save.borrow_mut();
            s.interactive_commands = out;
            let current = s.current;
            for tab in &mut s.tabs {
                if tab.terminal_mode != TerminalMode::InteractiveAi {
                    continue;
                }
                if tab.interactive_launcher_program.trim().is_empty() {
                    continue;
                }
                if tab.terminal_pinned_footer_override.is_some() {
                    continue;
                }
                tab.terminal_pinned_footer_lines =
                    pinned_footer_lines_for_specs(tab.interactive_launcher_program.as_str(), &specs);
                if let Some(spec) =
                    spec_for_program_in_specs(tab.interactive_launcher_program.as_str(), &specs)
                {
                    tab.interactive_markers = spec.markers.clone();
                    tab.interactive_archive_repainted_frames = spec.archive_repainted_frames;
                }
            }
            current < s.tabs.len() && s.tabs[current].terminal_mode == TerminalMode::InteractiveAi
        };
        let snapshot = st_save.borrow().interactive_commands.clone();
        match AppConfig::load_or_default() {
            Ok(mut cfg) => {
                cfg.set_interactive_commands(&snapshot);
                if let Err(e) = cfg.save() {
                    eprintln!("CliGJ: save config: {e}");
                }
            }
            Err(e) => eprintln!("CliGJ: load config: {e}"),
        }
        sync_interactive_command_choices_to_ui(&ui, &st_save.borrow());
        ui.set_ws_interactive_manage_open(false);
        if refresh_current_interactive {
            let mut s = st_save.borrow_mut();
            if s.current < s.tabs.len() {
                let current = s.current;
                ui.set_ws_terminal_pin_lines(SharedString::from(
                    s.tabs[current].terminal_pinned_footer_lines.to_string().as_str(),
                ));
                refresh_terminal_tab_view(&ui, &mut s.tabs[current]);
            }
        }
    });
}

fn parse_marker_editor_text(text: &str) -> Vec<String> {
    text.split([',', '\n'])
        .map(str::trim)
        .filter(|marker| !marker.is_empty())
        .map(ToString::to_string)
        .collect()
}
