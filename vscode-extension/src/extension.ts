import * as net from "node:net";
import * as vscode from "vscode";

const PIPE_PATH = "\\\\.\\pipe\\cligj-ipc-v1";
const REQUEST_TIMEOUT_MS = 2000;
const MAX_SELECTION_PAYLOAD_CHARS = 12000;

type IpcResponse = {
  type: "response";
  id?: number;
  ok: boolean;
  result?: unknown;
  error?: string;
};

type SelectionPromptData = {
  prompt: string;
  selectionPayloads: string[];
};

type ExplorerPathPromptData = {
  prompt: string;
  filePathPayloads: string[];
};

function sendRequest(method: string, params: Record<string, unknown> = {}): Promise<IpcResponse> {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(PIPE_PATH);
    const id = Date.now();
    let buffer = "";
    const timer = setTimeout(() => {
      socket.destroy();
      reject(new Error("IPC request timeout"));
    }, REQUEST_TIMEOUT_MS);

    socket.on("connect", () => {
      const payload = JSON.stringify({ id, method, params }) + "\n";
      socket.write(payload);
    });

    socket.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      const idx = buffer.indexOf("\n");
      if (idx < 0) {
        return;
      }
      const line = buffer.slice(0, idx).trim();
      clearTimeout(timer);
      socket.end();
      if (!line) {
        reject(new Error("Empty response from CliGJ"));
        return;
      }
      try {
        resolve(JSON.parse(line) as IpcResponse);
      } catch (err) {
        reject(err);
      }
    });

    socket.on("error", (err) => {
      clearTimeout(timer);
      reject(err);
    });
  });
}

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("CliGJ Bridge");
  context.subscriptions.push(output);

  const sendPromptPayload = async (
    prompt: string,
    submit: boolean,
    selectionPayloads: string[] = [],
    filePathPayloads: string[] = []
  ): Promise<void> => {
    output.appendLine(
      `[sendPrompt] submit=${submit} chars=${prompt.length} selectionPayloads=${selectionPayloads.length} filePathPayloads=${filePathPayloads.length}`
    );
    try {
      let resp = await sendRequest("sendPrompt", {
        prompt,
        submit,
        selectionPayloads,
        filePathPayloads
      });
      if (!resp.ok && (resp.error ?? "").includes("no active tab")) {
        output.appendLine("[sendPrompt] no active tab, trying openTab then retry");
        await sendRequest("openTab", { focus: true });
        resp = await sendRequest("sendPrompt", {
          prompt,
          submit,
          selectionPayloads,
          filePathPayloads
        });
      }
      if (resp.ok) {
        output.appendLine("[sendPrompt] success");
        void vscode.window.showInformationMessage(
          submit ? "Prompt sent to CliGJ" : "Prompt filled to CliGJ input box"
        );
      } else {
        output.appendLine(`[sendPrompt] failed: ${resp.error ?? "unknown error"}`);
        void vscode.window.showErrorMessage(`sendPrompt failed: ${resp.error ?? "unknown error"}`);
      }
    } catch (err) {
      output.appendLine(`[sendPrompt] error: ${String(err)}`);
      void vscode.window.showErrorMessage(`sendPrompt error: ${String(err)}`);
    }
  };

  const getSelectionForAi = (editor: vscode.TextEditor): SelectionPromptData | undefined => {
    const nonEmptySelections = editor.selections.filter((selection) => !selection.isEmpty);
    if (nonEmptySelections.length === 0) {
      return undefined;
    }

    const document = editor.document;
    const language = document.languageId || "text";
    const workspaceFolder = vscode.workspace.getWorkspaceFolder(document.uri);
    const filePath = workspaceFolder
      ? vscode.workspace.asRelativePath(document.uri, false)
      : document.uri.fsPath;

    const selectionPayloads: string[] = [];
    const promptLines: string[] = [`File: ${filePath}`];
    nonEmptySelections.forEach((selection, index) => {
      const startLine = selection.start.line;
      const endLine =
        selection.end.character === 0 && selection.end.line > selection.start.line
          ? selection.end.line - 1
          : selection.end.line;
      const selectedText = document.getText(selection).trimEnd();
      const safeText = selectedText.length > 0 ? selectedText : document.lineAt(startLine).text;
      const token = `[[sel${index + 1}]]`;
      const range = `L${startLine + 1}-L${endLine + 1}`;
      selectionPayloads.push(
        [
          `[[selection ${index + 1} file="${filePath}" range="${range}"]]`,
          `Range: ${range}`,
          `\`\`\`${language}`,
          safeText,
          "```",
          "[[/selection]]"
        ].join("\n")
      );
      promptLines.push(`Selection ${index + 1} (${range}): ${token}`);
    });

    const prompt = promptLines.join("\n");
    let totalPayloadChars = selectionPayloads.reduce((sum, block) => sum + block.length, 0);
    if (totalPayloadChars > MAX_SELECTION_PAYLOAD_CHARS) {
      const trimmed: string[] = [];
      let remaining = MAX_SELECTION_PAYLOAD_CHARS;
      for (const block of selectionPayloads) {
        if (remaining <= 0) {
          break;
        }
        if (block.length <= remaining) {
          trimmed.push(block);
          remaining -= block.length;
          continue;
        }
        trimmed.push(`${block.slice(0, remaining)}\n\n[truncated]`);
        remaining = 0;
      }
      selectionPayloads.length = 0;
      selectionPayloads.push(...trimmed);
      totalPayloadChars = selectionPayloads.reduce((sum, block) => sum + block.length, 0);
      output.appendLine(
        `[selection] payload truncated to ${totalPayloadChars} chars for better readability`
      );
    }

    return {
      prompt,
      selectionPayloads
    };
  };

  const getExplorerPathPrompt = (
    targetUri?: vscode.Uri,
    allUris?: readonly vscode.Uri[]
  ): ExplorerPathPromptData | undefined => {
    const uris = (allUris && allUris.length > 0 ? allUris : targetUri ? [targetUri] : [])
      .filter((uri) => uri.scheme === "file");
    if (uris.length === 0) {
      return undefined;
    }

    const uniquePaths = Array.from(new Set(uris.map((uri) => uri.fsPath)));
    const normalizedPaths = uniquePaths.map((fsPath) => {
      const uri = vscode.Uri.file(fsPath);
      const workspaceFolder = vscode.workspace.getWorkspaceFolder(uri);
      const raw = workspaceFolder ? vscode.workspace.asRelativePath(uri, false) : fsPath;
      return raw.replace(/\\/g, "/");
    });

    const nameCounts = new Map<string, number>();
    const promptLines = normalizedPaths.map((path) => {
      const fileName = path.split("/").pop() || path;
      const occurrence = (nameCounts.get(fileName) ?? 0) + 1;
      nameCounts.set(fileName, occurrence);
      return occurrence === 1 ? `@${fileName}` : `@${fileName}_${occurrence}`;
    });
    return {
      prompt: promptLines.join("\n"),
      filePathPayloads: normalizedPaths
    };
  };

  const askPrompt = async (title: string): Promise<string | undefined> => {
    return vscode.window.showInputBox({
      title,
      placeHolder: "Enter prompt text...",
      ignoreFocusOut: true
    });
  };

  const sendPromptWithMode = async (submit: boolean): Promise<void> => {
    const prompt = await askPrompt(
      submit ? "Send Prompt to CliGJ (Direct Submit)" : "Fill Prompt in CliGJ Input Box (Editable)"
    );
    if (!prompt) {
      return;
    }
    await sendPromptPayload(prompt, submit);
  };

  const ping = vscode.commands.registerCommand("cligj.ping", async () => {
    try {
      const resp = await sendRequest("ping");
      if (resp.ok) {
        void vscode.window.showInformationMessage("CliGJ IPC ping success");
      } else {
        void vscode.window.showErrorMessage(`CliGJ ping failed: ${resp.error ?? "unknown error"}`);
      }
    } catch (err) {
      void vscode.window.showErrorMessage(`CliGJ ping error: ${String(err)}`);
    }
  });

  const openTab = vscode.commands.registerCommand("cligj.openTab", async () => {
    try {
      const resp = await sendRequest("openTab", { focus: true });
      if (resp.ok) {
        void vscode.window.showInformationMessage("CliGJ openTab sent");
      } else {
        void vscode.window.showErrorMessage(`openTab failed: ${resp.error ?? "unknown error"}`);
      }
    } catch (err) {
      void vscode.window.showErrorMessage(`openTab error: ${String(err)}`);
    }
  });

  const sendPrompt = vscode.commands.registerCommand("cligj.sendPrompt", async () => {
    await sendPromptWithMode(true);
  });

  const fillPrompt = vscode.commands.registerCommand("cligj.fillPrompt", async () => {
    await sendPromptWithMode(false);
  });

  const sendSelectionPrompt = vscode.commands.registerCommand("cligj.sendSelectionPrompt", async () => {
    output.appendLine("[command] cligj.sendSelectionPrompt");
    const editor = vscode.window.activeTextEditor;
    if (!editor) {
      void vscode.window.showErrorMessage("No active editor");
      return;
    }

    const selectionData = getSelectionForAi(editor);
    if (!selectionData) {
      void vscode.window.showWarningMessage("Please select text first");
      return;
    }

    await sendPromptPayload(selectionData.prompt, true, selectionData.selectionPayloads);
  });

  const fillSelectionPrompt = vscode.commands.registerCommand("cligj.fillSelectionPrompt", async () => {
    output.appendLine("[command] cligj.fillSelectionPrompt");
    const editor = vscode.window.activeTextEditor;
    if (!editor) {
      void vscode.window.showErrorMessage("No active editor");
      return;
    }

    const selectionData = getSelectionForAi(editor);
    if (!selectionData) {
      void vscode.window.showWarningMessage("Please select text first");
      return;
    }

    await sendPromptPayload(selectionData.prompt, false, selectionData.selectionPayloads);
  });

  const sendExplorerPath = vscode.commands.registerCommand(
    "cligj.sendExplorerPath",
    async (targetUri?: vscode.Uri, allUris?: readonly vscode.Uri[]) => {
      output.appendLine("[command] cligj.sendExplorerPath");
      const pathData = getExplorerPathPrompt(targetUri, allUris);
      if (!pathData) {
        void vscode.window.showWarningMessage("No file path found from Explorer selection");
        return;
      }
      await sendPromptPayload(pathData.prompt, true, [], pathData.filePathPayloads);
    }
  );

  const fillExplorerPath = vscode.commands.registerCommand(
    "cligj.fillExplorerPath",
    async (targetUri?: vscode.Uri, allUris?: readonly vscode.Uri[]) => {
      output.appendLine("[command] cligj.fillExplorerPath");
      const pathData = getExplorerPathPrompt(targetUri, allUris);
      if (!pathData) {
        void vscode.window.showWarningMessage("No file path found from Explorer selection");
        return;
      }
      await sendPromptPayload(pathData.prompt, false, [], pathData.filePathPayloads);
    }
  );

  context.subscriptions.push(
    ping,
    openTab,
    sendPrompt,
    fillPrompt,
    sendSelectionPrompt,
    fillSelectionPrompt,
    sendExplorerPath,
    fillExplorerPath
  );
}

export function deactivate(): void {}
