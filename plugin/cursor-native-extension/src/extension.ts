import * as vscode from "vscode";
import {
  LanguageClient,
  State,
  type InitializeParams,
  type LanguageClientOptions,
  type ServerOptions,
} from "vscode-languageclient/node";

import {
  type NativeDiagnosticInput,
  admitNativeWorkspace,
  createNativeDiagnosticsPayload,
  batchAdmittedNativeDiagnosticDocuments,
  resolveInstalledTraceDecayBinary,
  traceDecayInitializationOptions,
  toLspDiagnosticSeverity,
} from "./nativeDiagnostics.js";

const TRACEDECAY_NATIVE_DIAGNOSTICS_METHOD = "tracedecay/nativeDiagnostics";
const TRACEDECAY_CONTEXT_PROJECTIONS = [
  { kind: "diagnostics", revision: 1 },
  { kind: "postEditImpact", revision: 1 },
  { kind: "affectedTests", revision: 1 },
  { kind: "testRunResults", revision: 1 },
];

let activeController: CursorNativeDiagnosticsController | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  if (!isCursorDesktop()) {
    return;
  }

  const controller = new CursorNativeDiagnosticsController();
  activeController = controller;
  context.subscriptions.push(controller);
  context.subscriptions.push(
    vscode.workspace.onDidChangeWorkspaceFolders(() => {
      void controller.reconcile();
    }),
  );
  context.subscriptions.push(
    vscode.workspace.onDidGrantWorkspaceTrust(() => {
      void controller.reconcile();
    }),
  );

  await controller.reconcile();
}

export async function deactivate(): Promise<void> {
  await activeController?.stop();
  activeController = undefined;
}

class CursorNativeDiagnosticsController implements vscode.Disposable {
  private disposed = false;
  private queue: Promise<void> = Promise.resolve();
  private session: CursorNativeDiagnosticsSession | undefined;

  public reconcile(): Promise<void> {
    return this.enqueue(async () => {
      await this.session?.stop();
      this.session = undefined;

      if (this.disposed) {
        return;
      }

      const admission = admitNativeWorkspace(
        vscode.workspace.isTrusted,
        vscode.workspace.workspaceFolders ?? [],
      );
      if (admission.state === "unavailable") {
        if (admission.reason === "workspace_root_count") {
          void vscode.window.showWarningMessage(
            "TraceDecay native diagnostics require exactly one workspace folder; multi-root workspaces are not supported yet.",
          );
        }
        return;
      }

      try {
        this.session = await CursorNativeDiagnosticsSession.start(admission.root);
      } catch (error) {
        void vscode.window.showWarningMessage(
          `TraceDecay native diagnostics could not start: ${String(error)}`,
        );
      }
    });
  }

  public stop(): Promise<void> {
    this.disposed = true;
    return this.enqueue(async () => {
      await this.session?.stop();
      this.session = undefined;
    });
  }

  public dispose(): void {
    void this.stop();
  }

  private enqueue(operation: () => Promise<void>): Promise<void> {
    this.queue = this.queue.catch(() => undefined).then(operation);
    return this.queue;
  }
}

class CursorNativeDiagnosticsSession implements vscode.Disposable {
  private disposed = false;
  private readonly diagnosticsSubscription: vscode.Disposable;
  private readonly documentSubscription: vscode.Disposable;
  private readonly stateSubscription: vscode.Disposable;

  private constructor(
    private readonly client: LanguageClient,
    private readonly workspaceFolder: vscode.WorkspaceFolder,
  ) {
    this.diagnosticsSubscription = vscode.languages.onDidChangeDiagnostics((event) => {
      this.sendChangedDiagnostics(event.uris);
    });
    this.documentSubscription = vscode.workspace.onDidOpenTextDocument((document) => {
      this.sendChangedDiagnostics([document.uri]);
    });
    this.stateSubscription = client.onDidChangeState((event) => {
      if (event.newState === State.Running) {
        this.sendCurrentDiagnostics();
      }
    });
  }

  public static async start(
    workspaceFolder: vscode.WorkspaceFolder,
  ): Promise<CursorNativeDiagnosticsSession> {
    const serverOptions: ServerOptions = {
      command: resolveTraceDecayBinary(),
      args: ["lsp", "bridge", "--stdio"],
      options: {
        cwd: workspaceFolder.uri.fsPath,
      },
    };
    const clientOptions: LanguageClientOptions = {
      documentSelector: [
        {
          scheme: "file",
          pattern: `${workspaceFolder.uri.fsPath.replaceAll("\\", "/")}/**`,
        },
      ],
      initializationOptions: traceDecayInitializationOptions,
      workspaceFolder,
    };
    const client = new TraceDecayLanguageClient(
      "tracedecayCursorNative",
      "TraceDecay Cursor Native Diagnostics",
      serverOptions,
      clientOptions,
    );
    await client.start();

    const session = new CursorNativeDiagnosticsSession(client, workspaceFolder);
    session.sendCurrentDiagnostics();
    return session;
  }

  public async stop(): Promise<void> {
    await this.disposeAsync();
  }

  public dispose(): void {
    void this.disposeAsync();
  }

  private async disposeAsync(): Promise<void> {
    if (this.disposed) {
      return;
    }
    this.disposed = true;
    this.diagnosticsSubscription.dispose();
    this.documentSubscription.dispose();
    this.stateSubscription.dispose();
    await this.client.stop();
  }

  private sendCurrentDiagnostics(): void {
    const uris = vscode.languages.getDiagnostics().map(([uri]) => uri);
    this.sendChangedDiagnostics(uris);
  }

  private sendChangedDiagnostics(uris: readonly vscode.Uri[]): void {
    if (this.disposed) {
      return;
    }
    for (const batch of batchAdmittedNativeDiagnosticDocuments(
      uris,
      (candidate) => isAdmittedWorkspaceDocument(candidate, this.workspaceFolder),
    )) {
      for (const uri of batch) {
        const document = vscode.workspace.textDocuments.find(
          (candidate) => candidate.uri.toString() === uri.toString(),
        );
        if (document === undefined) {
          continue;
        }
        const payload = createNativeDiagnosticsPayload(
          uri.toString(),
          document.version,
          vscode.languages.getDiagnostics(uri).map(toNativeDiagnosticInput),
        );
        void this.client
          .sendNotification(TRACEDECAY_NATIVE_DIAGNOSTICS_METHOD, payload)
          .catch((error: unknown) => {
            this.client.warn(
              `TraceDecay native diagnostic notification was not delivered: ${String(error)}`,
            );
          });
      }
    }
  }
}

class TraceDecayLanguageClient extends LanguageClient {
  protected override fillInitializeParams(params: InitializeParams): void {
    super.fillInitializeParams(params);
    const capabilities = params.capabilities as typeof params.capabilities & {
      experimental?: Record<string, unknown>;
    };
    capabilities.experimental = {
      ...capabilities.experimental,
      tracedecay: {
        opaqueExpansion: true,
        projections: TRACEDECAY_CONTEXT_PROJECTIONS,
        revision: 1,
      },
    };
  }
}

function isCursorDesktop(): boolean {
  return vscode.env.appName.toLocaleLowerCase().includes("cursor");
}

function resolveTraceDecayBinary(): string {
  const configured = vscode.workspace
    .getConfiguration("tracedecay")
    .get<string>("binaryPath")
    ?.trim();
  return resolveInstalledTraceDecayBinary(configured, process.env.TRACEDECAY_BIN);
}

function isAdmittedWorkspaceDocument(
  uri: vscode.Uri,
  workspaceFolder: vscode.WorkspaceFolder,
): boolean {
  return (
    uri.scheme === "file" &&
    vscode.workspace.getWorkspaceFolder(uri)?.uri.toString() === workspaceFolder.uri.toString()
  );
}

function toNativeDiagnosticInput(diagnostic: vscode.Diagnostic): NativeDiagnosticInput {
  const source = diagnostic.source?.trim();
  const code = diagnostic.code;
  const severity = toLspDiagnosticSeverity(diagnostic.severity);
  return {
    range: {
      start: diagnostic.range.start,
      end: diagnostic.range.end,
    },
    ...(severity === undefined ? {} : { severity }),
    ...(code === undefined ? {} : { code: diagnosticCode(code) }),
    ...(source === undefined || source.length === 0 ? {} : { source }),
    message: diagnostic.message,
    data: diagnostic.tags?.length
      ? { category: diagnostic.tags.join(",") }
      : undefined,
  };
}

function diagnosticCode(
  code: Exclude<vscode.Diagnostic["code"], undefined>,
): string | number {
  return typeof code === "object" ? code.value : code;
}
