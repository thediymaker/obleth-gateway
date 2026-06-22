import type { UsageLogEntry, SpanEntry } from "@/lib/obleth";

// A compact, read-only "receipt" of what the gateway did for a single test
// request. Assembled purely from already-recorded telemetry (usage log + flight
// recorder spans) — it never changes the request. Span names mirror the boon
// engine's real output:
//   boon:vision, boon:guardrails_input, boon:guardrails_output,
//   boon:structured_repair, boon:tool_loop, boon:tool_loop:iter:<n>
export interface TraceSummary {
  model: string;
  /** Canonical boon labels that actually fired (e.g. "vision", "tool_loop"). */
  boonsFired: string[];
  /** Number of tool-loop iterations (search / MCP turns). */
  toolLoopIters: number;
  /** Distinct tool names invoked during the tool loop. */
  toolsCalled: string[];
  cacheStatus: string;
  inputTokens: number;
  outputTokens: number;
  ttftMs: number;
  totalMs: number;
  costUsd: number;
  statusCode: number;
  /** Span names that ended in error (surfaces which stage failed). */
  errorStages: string[];
}

const BOON_PREFIX = "boon:";
const TOOL_ITER_PREFIX = "boon:tool_loop:iter:";

/** Pull tool names out of a tool-loop iteration span's attributes JSON. */
function toolsFromAttributes(raw: string): string[] {
  if (!raw) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!parsed || typeof parsed !== "object") return [];
  const attrs = parsed as Record<string, unknown>;
  const out: string[] = [];
  const push = (v: unknown) => {
    if (typeof v === "string" && v.trim()) out.push(v.trim());
  };
  const tools = attrs.tools ?? attrs.tool ?? attrs.name;
  if (Array.isArray(tools)) tools.forEach(push);
  else if (typeof tools === "string") tools.split(",").forEach(push);
  return out;
}

export function assembleTrace(
  log: UsageLogEntry | null,
  spans: SpanEntry[],
): TraceSummary {
  const fired = new Set<string>();
  const tools = new Set<string>();
  const errorStages: string[] = [];
  let toolLoopIters = 0;

  for (const s of spans) {
    if (s.status === "error") errorStages.push(s.span_name);

    if (s.span_name.startsWith(TOOL_ITER_PREFIX)) {
      toolLoopIters++;
      fired.add("tool_loop");
      toolsFromAttributes(s.attributes).forEach((t) => tools.add(t));
    } else if (s.span_name.startsWith(BOON_PREFIX)) {
      fired.add(s.span_name.slice(BOON_PREFIX.length));
    }
  }

  return {
    model: log?.model ?? "",
    boonsFired: [...fired],
    toolLoopIters,
    toolsCalled: [...tools],
    cacheStatus: log?.cache_status ?? "off",
    inputTokens: log?.input_tokens ?? 0,
    outputTokens: log?.output_tokens ?? 0,
    ttftMs: log?.ttft_ms ?? 0,
    totalMs: log?.total_ms ?? 0,
    costUsd: log?.cost_usd ?? 0,
    statusCode: log?.status_code ?? 0,
    errorStages,
  };
}
