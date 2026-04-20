# CliGJ VS Code Extension

Minimal VS Code extension scaffold for CliGJ IPC integration.

## Development

1. Install dependencies:

   ```bash
   npm install
   ```

2. Compile once:

   ```bash
   npm run compile
   ```

3. Press `F5` in VS Code to launch an Extension Development Host.
   - The launch config now runs `npm: compile` automatically before startup.

## Commands

- `CliGJ: Ping IPC`
- `CliGJ: Open Tab`
- `CliGJ: Send Prompt (Direct Submit)`
- `CliGJ: Fill Prompt (Editable)`
- `CliGJ: Send Selection with Line Numbers (Direct Submit)`
- `CliGJ: Fill Selection with Line Numbers (Editable)`
- `CliGJ: Send File Path (Direct Submit)`
- `CliGJ: Fill File Path (Editable)`

When text is selected in an editor, right-click to access selection commands directly from the editor context menu.
When files are selected in Explorer, right-click to send or fill file paths directly.

## Troubleshooting

If you see `command 'cligj.sendPrompt' not found`, the extension usually failed to activate because the compiled entry file does not exist yet.

Run these commands inside `vscode-extension`:

```bash
npm install
npm run compile
```
