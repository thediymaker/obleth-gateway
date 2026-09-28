import type { McpServerTestRow } from "@/lib/charo/mcp/types";
import type { DailyStatsView, McpServer, ModelRoute } from "@/lib/obleth";

/** The MCP page's pure logic: a server's state, who uses it, and its week of calls. */

export type McpState = "answering" | "old-sse" | "not-answering" | "off" | "checking";

export const MCP_STATE_LABEL: Record<McpState, string> = {
  answering: "Answering",
  "old-sse": "Old-style SSE",
  "not-answering": "Not answering",
  off: "Off",
  checking: "Checking…",
};

export function mcpState(server: Pick<McpServer, "enabled">, probe: McpServerTestRow | undefined): McpState {
  if (!server.enabled) return "off";
  if (!probe) return "checking";
  if (probe.status === "pass") return "answering";
  if (probe.status === "warn") return probe.tools?.length ? "answering" : "old-sse";
  return "not-answering";
}

export interface McpUsage {
  toolCalls: number;
  direct: number;
  errors: number;
  /** Mean milliseconds per call over the window, or null with no calls. */
  avgMs: number | null;
}

export function usageOf(stats: DailyStatsView | undefined, name: string): McpUsage {
  const item = stats?.items.find((i) => i.id === name);
  const sum = (k: string) => (item?.days ?? []).reduce((n, d) => n + (d.counts[k] ?? 0), 0);
  const toolCalls = sum("tool_calls");
  const direct = sum("direct");
  const calls = toolCalls + direct;
  return { toolCalls, direct, errors: sum("errors"), avgMs: calls ? Math.round(sum("ms") / calls) : null };
}

/** Models that can use tools at all: a model needs function calling for the tool loop. */
export function toolCapable(models: ModelRoute[]): ModelRoute[] {
  return models.filter((m) => m.model_type === "chat" && m.supports_function_calling);
}

export interface McpRow {
  server: McpServer;
  state: McpState;
  probe: McpServerTestRow | undefined;
  tools: number | null;
  usedBy: string[];
  usage: McpUsage;
}

export function buildMcpRows(servers: McpServer[], models: ModelRoute[], probes: Record<string, McpServerTestRow>, stats: DailyStatsView | undefined): McpRow[] {
  return servers.map((s) => {
    const probe = probes[s.name];
    return {
      server: s,
      state: mcpState(s, probe),
      probe,
      tools: probe?.tools?.length ?? null,
      usedBy: models.filter((m) => (m.tool_servers ?? []).includes(s.name)).map((m) => m.model_name).sort(),
      usage: usageOf(stats, s.name),
    };
  });
}

/** "3 servers · 2 answering, 1 not reachable · 23 tools · used by 4 models" */
export function mcpLine(rows: McpRow[]): string {
  if (rows.length === 0) return "No tool servers yet";
  const answering = rows.filter((r) => r.state === "answering").length;
  const down = rows.filter((r) => r.state === "not-answering").length;
  const off = rows.filter((r) => r.state === "off").length;
  const tools = rows.reduce((n, r) => n + (r.tools ?? 0), 0);
  const models = new Set(rows.flatMap((r) => r.usedBy)).size;
  return [
    `${rows.length} server${rows.length === 1 ? "" : "s"}`,
    [answering ? `${answering} answering` : null, down ? `${down} not answering` : null, off ? `${off} off` : null].filter(Boolean).join(", "),
    tools ? `${tools} tools` : null,
    `used by ${models} model${models === 1 ? "" : "s"}`,
  ].filter(Boolean).join(" · ");
}

/** "212 ms" from a probe's initialize and tools/list times. */
export function probeLatency(probe: McpServerTestRow | undefined): number | null {
  if (!probe?.latencyMs) return null;
  return probe.latencyMs.initialize + (probe.latencyMs.toolsList ?? 0);
}
