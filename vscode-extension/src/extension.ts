import * as net from "node:net";
import * as vscode from "vscode";

const PIPE_PATH = "\\\\.\\pipe\\cligj-ipc-v1";
const REQUEST_TIMEOUT_MS = 2000;

type IpcResponse = {
  type: "response";
  id?: number;
  ok: boolean;
  result?: unknown;
  error?: string;
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
    const prompt = await vscode.window.showInputBox({
      title: "Send Prompt to CliGJ",
      placeHolder: "Enter prompt text...",
      ignoreFocusOut: true
    });
    if (!prompt) {
      return;
    }
    try {
      const resp = await sendRequest("sendPrompt", { prompt, submit: true });
      if (resp.ok) {
        void vscode.window.showInformationMessage("Prompt sent to CliGJ");
      } else {
        void vscode.window.showErrorMessage(`sendPrompt failed: ${resp.error ?? "unknown error"}`);
      }
    } catch (err) {
      void vscode.window.showErrorMessage(`sendPrompt error: ${String(err)}`);
    }
  });

  context.subscriptions.push(ping, openTab, sendPrompt);
}

export function deactivate(): void {}
