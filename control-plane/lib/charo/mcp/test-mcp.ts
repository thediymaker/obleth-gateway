import type { CharoTool } from "@/lib/charo/tools/types";
import { obleth } from "@/lib/obleth";
import { probeServer } from "./probe";
import type { McpServerTestRow, McpTestResult } from "./types";

interface Args {
  /** null ⇒ every registered server. */
  servers: string[] | null;
}

function inertRow(server: string, status: "fail" | "skip", message: string): McpServerTestRow {
  return { server, status, message, serverInfo: null, protocolVersion: null, tools: null, latencyMs: null };
}

export const testMcpTool: CharoTool<Args, McpTestResult> = {
  name: "test_mcp",
  description:
    "Verify registered MCP servers end-to-end through the gateway: run the MCP handshake " +
    "(initialize + tools/list) against /mcp/{name} and report reachability, protocol info, and " +
    "each server's tools. Omit `servers` to sweep every registered server.",
  parameters: {
    type: "object",
    properties: {
      servers: {
        type: "array",
        items: { type: "string" },
        description: "Registered MCP server names to test. Omit to test all of them.",
      },
    },
    additionalProperties: false,
  },
  resultType: "mcp_test_result",
  requiresConfirmation: false, // handshake + tools/list is read-only

  parseArgs(raw: unknown): Args {
    const o = (raw ?? {}) as Record<string, unknown>;
    if (o.servers === undefined || o.servers === null) return { servers: null };
    if (!Array.isArray(o.servers)) throw new Error("`servers` must be an array of server names.");
    const servers = [
      ...new Set(
        o.servers
          .filter((s): s is string => typeof s === "string" && s.trim() !== "")
          .map((s) => s.trim()),
      ),
    ];
    return { servers: servers.length ? servers : null };
  },

  async run(args, ctx, emit): Promise<McpTestResult> {
    const registered = await obleth.listMcpServers();
    const byName = new Map(registered.map((s) => [s.name, s]));
    const targets = args.servers ?? registered.map((s) => s.name);

    const rows: McpServerTestRow[] = [];
    for (const name of targets) {
      if (ctx.signal.aborted) break;
      const reg = byName.get(name);
      if (!reg) {
        rows.push(inertRow(name, "fail", "not registered"));
        continue;
      }
      if (!reg.enabled) {
        rows.push(inertRow(name, "skip", "disabled in registry"));
        continue;
      }
      emit({ kind: "mcp_probe", server: name });
      rows.push(await probeServer(name, { signal: ctx.signal }));
    }
    return { servers: rows };
  },
};
