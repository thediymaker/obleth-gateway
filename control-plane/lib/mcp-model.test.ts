import { describe, expect, it } from "vitest";
import { buildMcpRows, mcpLine, mcpState, toolCapable, usageOf } from "./mcp-model";
import type { McpServerTestRow } from "./charo/mcp/types";
import type { DailyStatsView, McpServer, ModelRoute } from "./obleth";

const server = (name: string, enabled = true): McpServer => ({ id: name, name, upstream_url: `https://${name}.example/mcp`, auth_header_set: false, enabled, created_at: "", updated_at: "" });
const probe = (name: string, status: McpServerTestRow["status"], tools = 3): McpServerTestRow => ({ server: name, status, message: status === "fail" ? "connection refused" : "ok", serverInfo: null, protocolVersion: null, tools: status === "fail" ? null : Array.from({ length: tools }, (_, i) => ({ name: `t${i}`, description: null })), latencyMs: { initialize: 100, toolsList: 12 } });

describe("MCP servers", () => {
  it("reads a server's state from its check", () => {
    expect(mcpState(server("a"), probe("a", "pass"))).toBe("answering");
    expect(mcpState(server("a"), probe("a", "warn", 0))).toBe("old-sse");
    expect(mcpState(server("a"), probe("a", "fail"))).toBe("not-answering");
    expect(mcpState(server("a", false), undefined)).toBe("off");
    expect(mcpState(server("a"), undefined)).toBe("checking");
  });

  it("adds up a week of calls", () => {
    const stats: DailyStatsView = { kind: "mcp", days: [], items: [{ id: "github", days: [{ day: "2026-09-27", counts: { tool_calls: 10, direct: 2, errors: 1, ms: 1200 } }, { day: "2026-09-28", counts: { tool_calls: 4, ms: 400 } }] }] };
    expect(usageOf(stats, "github")).toEqual({ toolCalls: 14, direct: 2, errors: 1, avgMs: 100 });
    expect(usageOf(stats, "jira")).toEqual({ toolCalls: 0, direct: 0, errors: 0, avgMs: null });
  });

  it("says who uses what", () => {
    const models = [
      { model_name: "kimi", model_type: "chat", supports_function_calling: true, tool_servers: ["github", "jira"] },
      { model_name: "glm", model_type: "chat", supports_function_calling: true, tool_servers: ["jira"] },
      { model_name: "embed", model_type: "embedding", supports_function_calling: false, tool_servers: [] },
    ] as unknown as ModelRoute[];
    const rows = buildMcpRows([server("github"), server("jira")], models, { github: probe("github", "pass", 17), jira: probe("jira", "fail") }, undefined);
    expect(rows.map((r) => [r.server.name, r.state, r.tools, r.usedBy])).toEqual([
      ["github", "answering", 17, ["kimi"]],
      ["jira", "not-answering", null, ["glm", "kimi"]],
    ]);
    expect(mcpLine(rows)).toBe("2 servers · 1 answering, 1 not answering · 17 tools · used by 2 models");
    expect(toolCapable(models).map((m) => m.model_name)).toEqual(["kimi", "glm"]);
  });
});
