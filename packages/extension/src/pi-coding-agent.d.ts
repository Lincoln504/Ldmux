/**
 * Minimal ambient types for the pi extension host.
 *
 * pi is NOT a runtime dependency of this repo — it is the host process that
 * loads the extension (docs/PI-EXTENSIONS.md). Declaring only the surface ldmux
 * actually uses keeps `bun run typecheck` honest without pulling pi's package
 * into the buildless workspace. If we ever use more of the API, extend this
 * file deliberately (it is a contract, not a stub).
 */
declare module "@earendil-works/pi-coding-agent" {
  export interface ExtensionUi {
    notify(message: string, level?: "info" | "warning" | "error"): void;
  }

  export interface ExtensionSessionManager {
    getSessionId(): string;
    getName?(): string | undefined;
  }

  export interface ExtensionContext {
    hasUI: boolean;
    cwd: string;
    ui: ExtensionUi;
    sessionManager: ExtensionSessionManager;
  }

  export interface ExtensionToolResult {
    content: Array<{ type: "text"; text: string }>;
    details?: unknown;
    isError?: boolean;
  }

  export interface ExtensionToolDefinition {
    name: string;
    label?: string;
    description: string;
    parameters: unknown;
    execute(
      toolCallId: string,
      params: Record<string, unknown>,
      ...rest: unknown[]
    ): ExtensionToolResult | Promise<ExtensionToolResult>;
  }

  export interface ExtensionCommandDefinition {
    description: string;
    handler(args: string, ctx: ExtensionContext): void | Promise<void>;
  }

  export interface ExtensionAPI {
    on(event: string, handler: (event: any, ctx: ExtensionContext) => void | Promise<void>): void;
    registerTool(definition: ExtensionToolDefinition): void;
    registerCommand(name: string, definition: ExtensionCommandDefinition): void;
  }
}