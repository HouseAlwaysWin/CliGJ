import * as fs from "node:fs";
import { randomUUID } from "node:crypto";
import * as net from "node:net";
import * as path from "node:path";
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

type IpcEvent = {
  type: "event";
  event: string;
  data?: unknown;
};

type IpcMessage = IpcResponse | IpcEvent;

type SelectionPromptData = {
  prompt: string;
  selectionPayloads: string[];
  filePathPayloads: string[];
  fileOriginPayloads: FileOriginPayload[];
};

type ExplorerPathPromptData = {
  prompt: string;
  filePathPayloads: string[];
  fileOriginPayloads: FileOriginPayload[];
};

type FileOriginPayload = {
  clientId: string;
  uriScheme: string;
};

function sendRequest(method: string, params: Record<string, unknown> = {}): Promise<IpcResponse> {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(PIPE_PATH);
    const id = Date.now();
    let buffer = "";
    let settled = false;
    const timer = setTimeout(() => {
      settled = true;
      socket.destroy();
      reject(new Error("IPC request timeout"));
    }, REQUEST_TIMEOUT_MS);

    const finishWithError = (err: unknown): void => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      reject(err);
    };

    const finishWithResponse = (resp: IpcResponse): void => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      socket.end();
      resolve(resp);
    };

    const tryConsumeBuffer = (): void => {
      while (true) {
        const idx = buffer.indexOf("\n");
        if (idx < 0) {
          return;
        }
        const line = buffer.slice(0, idx).trim();
        buffer = buffer.slice(idx + 1);
        if (!line) {
          continue;
        }
        let msg: IpcMessage;
        try {
          msg = JSON.parse(line) as IpcMessage;
        } catch (err) {
          finishWithError(err);
          return;
        }
        if (msg.type === "event") {
          continue;
        }
        if (msg.id !== undefined && msg.id !== id) {
          continue;
        }
        finishWithResponse(msg);
        return;
      }
    };

    socket.on("connect", () => {
      const payload = JSON.stringify({ id, method, params }) + "\n";
      socket.write(payload);
    });

    socket.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      tryConsumeBuffer();
    });

    socket.on("end", () => {
      if (!settled) {
        finishWithError(new Error("CliGJ closed IPC connection before sending a response"));
      }
    });

    socket.on("error", (err) => {
      finishWithError(err);
    });
  });
}

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("CliGJ Bridge");
  context.subscriptions.push(output);
  const clientId = randomUUID();
  const currentFileOrigin = (): FileOriginPayload => ({
    clientId,
    uriScheme: vscode.env.uriScheme
  });

  const hasNonEmptySelection = (editor: vscode.TextEditor | undefined): boolean =>
    !!editor && editor.selections.some((selection) => !selection.isEmpty);

  const sendSelectionStatus = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 110);
  sendSelectionStatus.name = "CliGJ Send Selection";
  sendSelectionStatus.command = "cligj.sendSelectionPrompt";
  sendSelectionStatus.text = "$(send) CliGJ Send";
  sendSelectionStatus.tooltip = "Send selection to CliGJ (Direct Submit) — Ctrl+Alt+S";

  const fillSelectionStatus = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 109);
  fillSelectionStatus.name = "CliGJ Fill Selection";
  fillSelectionStatus.command = "cligj.fillSelectionPrompt";
  fillSelectionStatus.text = "$(edit) CliGJ Fill";
  fillSelectionStatus.tooltip = "Fill selection to CliGJ input (Editable) — Ctrl+Alt+F";

  const updateSelectionStatusBar = (): void => {
    if (hasNonEmptySelection(vscode.window.activeTextEditor)) {
      sendSelectionStatus.show();
      fillSelectionStatus.show();
      return;
    }
    sendSelectionStatus.hide();
    fillSelectionStatus.hide();
  };

  const fileNameFromPath = (path: string): string => {
    const normalized = path.replace(/\\/g, "/");
    const parts = normalized.split("/");
    return parts[parts.length - 1] || normalized;
  };

  const parsePositiveInt = (value: string | null): number | undefined => {
    if (!value) {
      return undefined;
    }
    const parsed = Number.parseInt(value, 10);
    return Number.isFinite(parsed) && parsed > 0 ? parsed : undefined;
  };

  const resolveTargetFsPath = (rawPath: string): string => {
    if (path.isAbsolute(rawPath)) {
      return rawPath;
    }
    const folders = vscode.workspace.workspaceFolders ?? [];
    for (const folder of folders) {
      const candidate = path.join(folder.uri.fsPath, rawPath);
      if (fs.existsSync(candidate)) {
        return candidate;
      }
    }
    if (folders.length > 0) {
      return path.join(folders[0].uri.fsPath, rawPath);
    }
    return rawPath;
  };

  const openEditorTarget = async (
    rawPath: string,
    startLine?: number,
    endLine?: number
  ): Promise<void> => {
    const fsPath = resolveTargetFsPath(rawPath);
    const document = await vscode.workspace.openTextDocument(vscode.Uri.file(fsPath));
    const editor = await vscode.window.showTextDocument(document, {
      preview: false,
      preserveFocus: false
    });

    if (startLine === undefined || document.lineCount <= 0) {
      return;
    }

    const startLineIndex = Math.min(Math.max(startLine - 1, 0), document.lineCount - 1);
    const endLineIndex = Math.min(
      Math.max((endLine ?? startLine) - 1, startLineIndex),
      document.lineCount - 1
    );
    const start = new vscode.Position(startLineIndex, 0);
    const end = document.lineAt(endLineIndex).range.end;
    const selection = new vscode.Selection(start, end);
    editor.selection = selection;
    editor.selections = [selection];
    editor.revealRange(new vscode.Range(start, end), vscode.TextEditorRevealType.InCenter);
  };

  const sendPromptPayload = async (
    prompt: string,
    submit: boolean,
    selectionPayloads: string[] = [],
    filePathPayloads: string[] = [],
    fileOriginPayloads: FileOriginPayload[] = []
  ): Promise<void> => {
    output.appendLine(
      `[sendPrompt] submit=${submit} chars=${prompt.length} selectionPayloads=${selectionPayloads.length} filePathPayloads=${filePathPayloads.length} fileOriginPayloads=${fileOriginPayloads.length}`
    );
    try {
      let resp = await sendRequest("sendPrompt", {
        prompt,
        submit,
        selectionPayloads,
        filePathPayloads,
        fileOriginPayloads
      });
      if (!resp.ok && (resp.error ?? "").includes("no active tab")) {
        output.appendLine("[sendPrompt] no active tab, trying openTab then retry");
        await sendRequest("openTab", { focus: true });
        resp = await sendRequest("sendPrompt", {
          prompt,
          submit,
          selectionPayloads,
          filePathPayloads,
          fileOriginPayloads
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
    const normalizedFilePath = filePath.replace(/\\/g, "/");
    const fileToken = `@${fileNameFromPath(normalizedFilePath)}`;

    const selectionPayloads: string[] = [];
    const promptParts: string[] = [];
    nonEmptySelections.forEach((selection, index) => {
      const startLine = selection.start.line;
      const endLine =
        selection.end.character === 0 && selection.end.line > selection.start.line
          ? selection.end.line - 1
          : selection.end.line;
      const selectedText = document.getText(selection).trimEnd();
      const safeText = selectedText.length > 0 ? selectedText : document.lineAt(startLine).text;
      const range = `L${startLine + 1}-L${endLine + 1}`;
      selectionPayloads.push(
        [
          `[[selection ${index + 1} file="${normalizedFilePath}" range="${range}"]]`,
          `Range: ${range}`,
          `\`\`\`${language}`,
          safeText,
          "```",
          "[[/selection]]"
        ].join("\n")
      );
      const lineLabel =
        startLine === endLine ? `${startLine + 1}` : `${startLine + 1}-${endLine + 1}`;
      promptParts.push(`${fileToken} (${lineLabel})`);
    });

    const prompt = promptParts.join(" | ");
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
      selectionPayloads,
      filePathPayloads: [normalizedFilePath],
      fileOriginPayloads: [currentFileOrigin()]
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
      // Keep one space between file tokens for multi-select explorer sends.
      prompt: promptLines.join(" "),
      filePathPayloads: normalizedPaths,
      fileOriginPayloads: normalizedPaths.map(() => currentFileOrigin())
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

  const uriHandler: vscode.UriHandler = {
    handleUri: async (uri: vscode.Uri): Promise<void> => {
      output.appendLine(`[uri] ${uri.toString(true)}`);
      const action = uri.path.replace(/^\/+/, "");
      if (action !== "openSelection") {
        output.appendLine(`[uri] ignored unsupported action: ${action}`);
        return;
      }

      const params = new URLSearchParams(uri.query);
      const rawPath = params.get("path");
      if (!rawPath) {
        void vscode.window.showErrorMessage("CliGJ openSelection URI is missing path");
        return;
      }

      const startLine = parsePositiveInt(params.get("startLine"));
      const endLine = parsePositiveInt(params.get("endLine"));

      try {
        await openEditorTarget(rawPath, startLine, endLine);
      } catch (err) {
        output.appendLine(`[uri] openSelection error: ${String(err)}`);
        void vscode.window.showErrorMessage(`CliGJ openSelection failed: ${String(err)}`);
      }
    }
  };

  const handleServerEvent = async (event: IpcEvent): Promise<void> => {
    if (event.event !== "openEditorLocation" || !event.data || typeof event.data !== "object") {
      return;
    }
    const data = event.data as Record<string, unknown>;
    const targetClientId =
      typeof data.clientId === "string" ? data.clientId : "";
    if (targetClientId !== clientId) {
      return;
    }
    const targetPath =
      typeof data.path === "string" ? data.path : "";
    if (!targetPath) {
      return;
    }
    const startLine =
      typeof data.startLine === "number" && Number.isFinite(data.startLine) && data.startLine > 0
        ? data.startLine
        : undefined;
    const endLine =
      typeof data.endLine === "number" && Number.isFinite(data.endLine) && data.endLine > 0
        ? data.endLine
        : undefined;
    output.appendLine(
      `[event] openEditorLocation clientId=${targetClientId} path=${targetPath} start=${String(startLine)} end=${String(endLine)}`
    );
    await openEditorTarget(targetPath, startLine, endLine);
  };

  const startEventSubscription = (): vscode.Disposable => {
    let socket: net.Socket | undefined;
    let reconnectTimer: NodeJS.Timeout | undefined;
    let disposed = false;
    let buffer = "";

    const clearReconnect = (): void => {
      if (reconnectTimer) {
        clearTimeout(reconnectTimer);
        reconnectTimer = undefined;
      }
    };

    const scheduleReconnect = (): void => {
      if (disposed || reconnectTimer) {
        return;
      }
      reconnectTimer = setTimeout(() => {
        reconnectTimer = undefined;
        connect();
      }, 1000);
    };

    const tryConsumeBuffer = (): void => {
      while (true) {
        const idx = buffer.indexOf("\n");
        if (idx < 0) {
          return;
        }
        const line = buffer.slice(0, idx).trim();
        buffer = buffer.slice(idx + 1);
        if (!line) {
          continue;
        }
        let msg: IpcMessage;
        try {
          msg = JSON.parse(line) as IpcMessage;
        } catch (err) {
          output.appendLine(`[event] invalid JSON: ${String(err)}`);
          continue;
        }
        if (msg.type === "event") {
          void handleServerEvent(msg);
        }
      }
    };

    const connect = (): void => {
      if (disposed) {
        return;
      }
      clearReconnect();
      buffer = "";
      socket?.destroy();
      socket = net.createConnection(PIPE_PATH);

      socket.on("connect", () => {
        output.appendLine(`[event] subscribed clientId=${clientId} uriScheme=${vscode.env.uriScheme}`);
        socket?.write(
          JSON.stringify({
            id: Date.now(),
            method: "subscribe",
            params: { clientId, uriScheme: vscode.env.uriScheme }
          }) + "\n"
        );
      });

      socket.on("data", (chunk) => {
        buffer += chunk.toString("utf8");
        tryConsumeBuffer();
      });

      socket.on("error", (err) => {
        output.appendLine(`[event] socket error: ${String(err)}`);
      });

      socket.on("close", () => {
        socket = undefined;
        scheduleReconnect();
      });

      socket.on("end", () => {
        socket = undefined;
        scheduleReconnect();
      });
    };

    connect();

    return new vscode.Disposable(() => {
      disposed = true;
      clearReconnect();
      socket?.destroy();
      socket = undefined;
    });
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

    await sendPromptPayload(
      selectionData.prompt,
      true,
      selectionData.selectionPayloads,
      selectionData.filePathPayloads,
      selectionData.fileOriginPayloads
    );
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

    await sendPromptPayload(
      selectionData.prompt,
      false,
      selectionData.selectionPayloads,
      selectionData.filePathPayloads,
      selectionData.fileOriginPayloads
    );
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
      await sendPromptPayload(
        pathData.prompt,
        true,
        [],
        pathData.filePathPayloads,
        pathData.fileOriginPayloads
      );
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
      await sendPromptPayload(
        pathData.prompt,
        false,
        [],
        pathData.filePathPayloads,
        pathData.fileOriginPayloads
      );
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
    fillExplorerPath,
    startEventSubscription(),
    vscode.window.registerUriHandler(uriHandler),
    sendSelectionStatus,
    fillSelectionStatus,
    vscode.window.onDidChangeActiveTextEditor(() => updateSelectionStatusBar()),
    vscode.window.onDidChangeTextEditorSelection((event) => {
      if (event.textEditor === vscode.window.activeTextEditor) {
        updateSelectionStatusBar();
      }
    })
  );

  updateSelectionStatusBar();
}

export function deactivate(): void {}
