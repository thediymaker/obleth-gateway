import { describe, it, expect } from "vitest";
import { assembleTrace } from "./trace";
import type { UsageLogEntry, SpanEntry } from "@/lib/obleth";

function span(name: string, extra: Partial<SpanEntry> = {}): SpanEntry {
  return {
    request_id: "rid",
    span_name: name,
    parent_span: "proxy_request",
    start_ms: 0,
    duration_ms: 1,
    status: "ok",
    attributes: "{}",
    ...extra,
  };
}

const log: UsageLogEntry = {
  request_id: "rid",
  ts_ms: 0,
  tenant_id: "t",
  key_id: "k",
  model: "llama-3",
  request_type: "chat",
  session_id: "",
  session_id_source: "",
  admission: "ok",
  status_code: 200,
  input_tokens: 12,
  output_tokens: 30,
  total_tokens: 42,
  queue_wait_ms: 0,
  ttft_ms: 90,
  total_ms: 240,
  cache_status: "miss",
  cost_usd: 0.001,
  energy_wh: 0,
  energy_cost_usd: 0,
  co2_g: 0,
  tenant_name: "__control_plane__",
  key_name: "charo",
  key_prefix: "ob_",
  has_trace: true,
};

describe("assembleTrace", () => {
  it("combines usage log fields with boon spans", () => {
    const spans: SpanEntry[] = [
      span("boon:vision"),
      span("boon:guardrails_input"),
      span("boon:tool_loop"),
      span("boon:tool_loop:iter:0", { attributes: JSON.stringify({ tools: ["web_search"] }) }),
      span("boon:tool_loop:iter:1", { attributes: JSON.stringify({ tools: ["mcp.fetch_url"] }) }),
    ];
    const t = assembleTrace(log, spans);
    expect(t.model).toBe("llama-3");
    expect(t.boonsFired).toContain("vision");
    expect(t.boonsFired).toContain("guardrails_input");
    expect(t.boonsFired).toContain("tool_loop");
    expect(t.toolLoopIters).toBe(2);
    expect(t.toolsCalled).toEqual(["web_search", "mcp.fetch_url"]);
    expect(t.outputTokens).toBe(30);
    expect(t.cacheStatus).toBe("miss");
    expect(t.errorStages).toEqual([]);
  });

  it("records error stages and tolerates a missing log", () => {
    const spans: SpanEntry[] = [
      span("boon:guardrails_input", { status: "error" }),
      span("upstream", { status: "error" }),
    ];
    const t = assembleTrace(null, spans);
    expect(t.model).toBe("");
    expect(t.statusCode).toBe(0);
    expect(t.errorStages).toEqual(["boon:guardrails_input", "upstream"]);
  });

  it("parses comma-separated tool attributes", () => {
    const spans: SpanEntry[] = [
      span("boon:tool_loop:iter:0", { attributes: JSON.stringify({ tools: "a, b" }) }),
    ];
    const t = assembleTrace(log, spans);
    expect(t.toolsCalled).toEqual(["a", "b"]);
  });
});
