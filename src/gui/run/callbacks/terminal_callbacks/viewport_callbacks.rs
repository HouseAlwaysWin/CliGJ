use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use slint::{spawn_local, ComponentHandle, Timer};

use crate::gui::slint_ui::AppWindow;
use crate::gui::state::{GuiState, TerminalMode};
use crate::gui::ui_sync::{
    clamp_saved_scroll_top, push_terminal_view_to_ui, scrollable_terminal_line_count,
    terminal_scroll_top_for_tab, TERMINAL_ROW_HEIGHT_PX, UI_LAYOUT_EPOCH,
};
use crate::terminal::types::ControlCommand;

const TERMINAL_RESIZE_DEBOUNCE_MS: u64 = 140;

pub(super) fn connect_terminal_resize(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_resize = Rc::clone(&state);
    let app_weak = app.as_weak();
    let pending_resize = Rc::new(RefCell::new(None::<(u64, u64, i32, i32)>));
    let resize_timer = Rc::new(Timer::default());

    app.on_terminal_resize_requested(move |cols, rows| {
        if cols <= 0 || rows <= 0 {
            return;
        }

        let request_epoch = UI_LAYOUT_EPOCH.with(|c| c.get());
        let target_tab_id = crate::gui::ui_sync::RESIZE_TARGET_TAB_ID.with(|c| c.get());
        *pending_resize.borrow_mut() = Some((request_epoch, target_tab_id, cols, rows));

        let app_weak2 = app_weak.clone();
        let st_resize2 = Rc::clone(&st_resize);
        let pending_resize2 = Rc::clone(&pending_resize);
        resize_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(TERMINAL_RESIZE_DEBOUNCE_MS),
            move || {
                let Some((request_epoch, target_tab_id, cols, rows)) =
                    pending_resize2.borrow_mut().take()
                else {
                    return;
                };
                let Some(_ui) = app_weak2.upgrade() else {
                    return;
                };
                if UI_LAYOUT_EPOCH.with(|c| c.get()) != request_epoch {
                    return;
                }

                let mut s = st_resize2.borrow_mut();
                let Some(tab) = s.tabs.iter_mut().find(|t| t.id == target_tab_id) else {
                    return;
                };
                if tab.last_pty_cols == cols as u16 && tab.last_pty_rows == rows as u16 {
                    return;
                }
                tab.last_pty_cols = cols as u16;
                tab.last_pty_rows = rows as u16;

                if let Some(tx) = &tab.pty_control_tx {
                    let _ = tx.send(ControlCommand::Resize {
                        cols: cols as u16,
                        rows: rows as u16,
                    });
                }
            },
        );
    });
}

pub(super) fn connect_terminal_wheel(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_wheel = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_terminal_wheel(move |delta| {
        let Some(ui) = app_weak.upgrade() else {
            return false;
        };
        let (handled, next_scroll) = {
            let mut s = st_wheel.borrow_mut();
            if s.current >= s.tabs.len() {
                return false;
            }
            if s.tabs[s.current].terminal_mode != TerminalMode::InteractiveAi {
                return false;
            }
            let current = s.current;
            let tab = &mut s.tabs[current];
            let vh = ui.get_ws_terminal_viewport_height_px().max(1.0);
            let max_scroll =
                ((scrollable_terminal_line_count(tab) as f32) * TERMINAL_ROW_HEIGHT_PX - vh)
                    .max(0.0);
            let current = if tab.interactive_follow_output {
                terminal_scroll_top_for_tab(tab, vh)
            } else {
                clamp_saved_scroll_top(tab, vh)
            };
            let steps = ((delta.abs() as f32) / 120.0).max(1.0).min(4.0);
            let amount = TERMINAL_ROW_HEIGHT_PX * 3.0 * steps;
            let mut next = if delta > 0 {
                (current - amount).max(0.0)
            } else if delta < 0 {
                (current + amount).min(max_scroll)
            } else {
                current
            };
            if max_scroll <= 0.5 {
                next = 0.0;
            }
            tab.interactive_follow_output = next >= (max_scroll - 1.0);
            tab.terminal_saved_scroll_top_px = next;
            (true, next)
        };
        if !handled {
            return false;
        }
        ui.invoke_ws_apply_terminal_scroll_top_px(next_scroll);
        let mut s = st_wheel.borrow_mut();
        if s.current >= s.tabs.len() {
            return false;
        }
        let current = s.current;
        let tab = &mut s.tabs[current];
        if tab.terminal_mode != TerminalMode::InteractiveAi {
            return false;
        }
        if (tab.terminal_saved_scroll_top_px - next_scroll).abs() > 0.5 {
            return true;
        }
        push_terminal_view_to_ui(&ui, tab, Some(next_scroll));
        true
    });
}

pub(super) fn connect_terminal_viewport(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_vp = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_terminal_viewport_changed(move || {
        let request_epoch = UI_LAYOUT_EPOCH.with(|c| c.get());
        let app_weak2 = app_weak.clone();
        let st_vp2 = Rc::clone(&st_vp);
        // Defer to after this Slint callback returns: avoids `RefCell` reborrow when
        // `viewport-changed` fires during another handler that already borrowed `state`.
        let _ = spawn_local(async move {
            let Some(ui) = app_weak2.upgrade() else {
                return;
            };
            if UI_LAYOUT_EPOCH.with(|c| c.get()) != request_epoch {
                return;
            }
            let mut s = st_vp2.borrow_mut();
            if s.current >= s.tabs.len() {
                return;
            }
            let cur = s.current;
            let tab = &mut s.tabs[cur];
            push_terminal_view_to_ui(&ui, tab, None);
        });
    });
}
