export type McpTestStatus = "pass" | "warn" | "fail" | "skip";

export interface McpToolInfo {
  name: string;
  description: string | null;
}

export interface McpServerTestRow {
  /** Registered server name (the /mcp/{name} path segment). */
  server: string;
  status: McpTestStatus;
  /** One-line verdict, e.g. "handshake OK — 12 tools" or the failure reason. */
  message: string;
  serverInfo: { name: string; version: string } | null;
  protocolVersion: string | null;
  /** null when tools/list never succeeded. */
  tools: McpToolInfo[] | null;
  latencyMs: { initialize: number; toolsList: number | null } | null;
}

export interface McpTestResult {
  servers: McpServerTestRow[];
}
