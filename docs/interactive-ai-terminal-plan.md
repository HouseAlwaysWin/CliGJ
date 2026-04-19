# Interactive AI Terminal Plan

## Goal

Split the current shared terminal synchronization model into two modes so normal shell sessions and interactive AI CLIs do not interfere with each other.

- `Shell`: keep the existing scrollback-oriented terminal behavior.
- `InteractiveAi`: treat Gemini/Codex/Claude/Copilot-style sessions as redraw-driven interactive UIs rather than normal shell history streams.

This plan exists because the current shared model assumes PTY output is append-like shell history. That assumption breaks when interactive AI CLIs redraw their whole screen on resize and push duplicated content into scrollback.

## Current Problem

The current pipeline is shared by all PTY-backed tabs:

1. ConPTY / wezterm produces a snapshot.
2. The snapshot is merged into `terminal_lines`.
3. The UI treats the result like shell scrollback.
4. Resize, scroll, and tab switching all reuse the same rendering path.

That model works for `Command Prompt` / `PowerShell`, but not for Gemini-like TUIs. On resize, those apps often redraw the full screen, and the redraw is incorrectly treated as new history.

## Target Architecture

Introduce a per-tab terminal mode:

- `Shell`
- `InteractiveAi`

The two modes share the same PTY transport, but not the same merge semantics.

### Shell Mode

Keep the existing behavior:

- append-like scrollback
- windowed rendering
- PTY resize support
- normal wheel / viewport history

### InteractiveAi Mode

Use a separate frame-oriented model:

- treat updates as current-screen snapshots
- do not interpret redraws as new history
- resize updates the visible frame rather than shell-like scrollback
- UI may show limited history or only the current frame in the first iteration

## Minimal Viable Version

The first version does not need perfect interactive scrollback.

It only needs to guarantee:

1. Gemini-style tabs do not duplicate content on resize.
2. Shell tabs keep their current behavior.
3. Tab switching still stays correct.

That means the first implementation can simplify interactive history behavior if necessary.

## Files To Change

### 1. `src/gui/state.rs`

Add the terminal mode and separate the data needed for interactive sessions.

Planned changes:

- add a `TerminalMode` enum
- add a `terminal_mode` field to `TabState`
- keep existing shell-oriented fields for `Shell`
- add frame-oriented storage for `InteractiveAi`
- keep the rest of `TabState` stable unless the mode actually requires different state

### 2. `src/gui/run/callbacks.rs`

Decide which tabs use which mode.

Planned changes:

- when `AI Models` launches `gemini`, `codex`, `claude`, or `copilot`, set the tab to `InteractiveAi`
- when a tab goes back to normal shell usage, restore `Shell`
- keep resize callbacks mode-aware
- avoid reintroducing tab-switch regressions while doing this

### 3. `src/terminal/windows_conpty.rs`

Make terminal output mode-aware at the source.

Planned changes:

- extend `TerminalRender` so it can represent either:
  - shell-oriented diff/update data
  - interactive full-frame snapshot data
- keep shell output behavior as close as possible to the current implementation
- for interactive mode, generate full-frame snapshots instead of trying to preserve scrollback semantics through resize redraws

### 4. `src/gui/run/timers.rs`

Split merge logic by mode.

Planned changes:

- `PendingTabUpdate` becomes mode-aware
- `apply_pending_updates()` branches:
  - `Shell`: existing merge logic
  - `InteractiveAi`: replace current frame rather than append redraws into history
- keep shell scrollback cap and cache maintenance unchanged for shell tabs

### 5. `src/gui/ui_sync.rs`

Push the right data model to the UI depending on the mode.

Planned changes:

- `Shell`: keep overscan/windowing behavior
- `InteractiveAi`: push current frame directly
- avoid shell-style assumptions such as scrollback-based row offsets for interactive tabs

### 6. `ui/components/gj_viewer.slint`

Only adjust viewer logic if needed after the data model split.

Possible follow-up changes:

- add a mode property
- reduce or customize scroll behavior for interactive mode
- keep shell mode untouched as much as possible

This should be a second-stage change, not the first thing to edit.

## Recommended Implementation Order

### Step 1

Introduce `TerminalMode` and add it to `TabState`.

Do not change behavior yet.

### Step 2

Mark `AI Models` sessions as `InteractiveAi`.

Keep all other tabs on `Shell`.

### Step 3

Extend the terminal update payloads so reader output can express:

- shell diff updates
- interactive frame updates

### Step 4

Implement the minimal `InteractiveAi` merge path in `timers.rs`.

This should be frame-replace oriented, not scrollback-append oriented.

### Step 5

Update `ui_sync.rs` so interactive tabs render from the current frame model instead of the shell history model.

### Step 6

Only after the above is stable, adjust `gj_viewer.slint` for mode-specific behavior if needed.

## Acceptance Criteria

### Shell tabs

- tab switching has no stale content
- resize behaves as before
- scrollback continues to work

### Interactive AI tabs

- width resize does not duplicate banners/logs
- height resize stays stable within the enforced minimum height
- redraws are not treated as new shell history

### Cross-mode safety

- switching between shell tabs and interactive AI tabs does not leak state
- fixing interactive behavior must not regress shell behavior

## Risks

### 1. Interactive history may be simplified first

The first version may intentionally prefer correctness over full history behavior.

### 2. Current code is coupled

`windows_conpty.rs`, `timers.rs`, and `ui_sync.rs` currently assume one merge model. Splitting that cleanly requires discipline to avoid half-shell, half-interactive hybrid logic.

### 3. Gemini may mix redraw and real output

If Gemini emits both true historical output and screen redraws in the same mode, the first implementation should prioritize preventing duplication, even if that means interactive history is conservative.

## Suggested Next Session Prompt

Use this next time to continue directly:

`照 interactive-ai-terminal-plan.md 的最小可用版開始做，先加 TerminalMode，並把 AI Models 分頁分流成 InteractiveAi。`
