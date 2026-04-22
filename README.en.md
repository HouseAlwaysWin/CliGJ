# CliGJ Project Overview

CliGJ is a desktop tool built with Rust + Slint. It is designed to unify local terminal workflows and AI-assisted command interactions in one interface, with IPC integration for VS Code so you can move quickly between editor context and terminal execution.

中文版本: `README.md`

## Core Features

### 1) Multi-tab Terminal Workspace

- Multiple terminal tabs with rename and reorder support.
- Each tab can use a different shell profile (Command Prompt, PowerShell, custom commands).
- Startup settings management (default shell, UI language, fonts, workspace path).

### 2) Prompt Composer and Context Injection

- Built-in prompt input area with submit mode and fill-edit mode.
- Supports injecting:
  - file paths
  - images
  - `@` workspace file picker results
- Context chips can be managed individually or cleared in bulk.

### 3) Interactive AI Command Integration

- Manage built-in and custom interactive commands (Gemini, Codex, Claude, Copilot, etc.).
- Supports pinned footer line control and quick switching.
- Raw / non-raw input mode switching to balance TTY passthrough and line-based editing.

### 4) IPC Bridge (Desktop App <-> VS Code)

- Built-in IPC server that allows external tools to:
  - open a new tab
  - send a prompt
  - fill a prompt
- IPC status is visible in the UI (ON/OFF, client count, error text).

### 5) VS Code Extension Collaboration

- From editor selection, you can directly:
  - Send Selection (direct submit)
  - Fill Selection (editable input first)
- Explorer multi-file context menu can send/fill file paths.
- Includes shortcuts and dynamic status bar buttons (shown when text is selected).

### 6) Update Support

- In-app "Check for updates / Switch version" UI (for Windows ZIP release assets).
- Update flow includes version selection, download, new app launch, and switch guidance.

## Typical Workflow

1. Open a workspace tab in CliGJ.
2. Select code in VS Code and send/fill it to CliGJ prompt.
3. Iterate through AI interactions and terminal output.
4. Inject files/images as needed to refine debugging or refactoring context.

## License

- This project is licensed under `GPL-3.0-or-later`.
- See the root `LICENSE` file for details.

