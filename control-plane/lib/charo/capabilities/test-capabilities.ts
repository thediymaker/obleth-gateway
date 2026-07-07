import type { CharoTool, ToolCtx, ToolProgress } from "@/lib/charo/tools/types";
import type { ChatMessage } from "@/lib/charo/gateway";
import { assembleTrace, type TraceSummary } from "@/lib/charo/trace";
import { obleth } from "@/lib/obleth";
import { TEST_CATALOG, FIXTURE_IMAGE_DATA_URL, evaluateTest } from "./catalog";
import type { TestId, CapabilityResult, TestOutcome } from "./types";

const ALL_TESTS: TestId[] = ["ping", "tools", "json", "vision"];

interface Args { model: string; tests: TestId[] }

/** Non-streaming assistant content from an OpenAI-style completion body. */
function contentOf(body: unknown): string {
  const choices = (body as { choices?: unknown })?.choices;
  if (!Array.isArray(choices) || !choices.length) return "";
  const msg = (choices[0] as { message?: { content?: unknown } }).message;
  return typeof msg?.content === "string" ? msg.content : "";
}

// Telemetry flushes on a ~1s ticker, so a trace isn't queryable the instant the
// request returns. Poll briefly; return null if it never lands (evaluateTest
// degrades to a WARN for the tools test rather than a false FAIL).
async function fetchTrace(requestId: string, signal: AbortSignal): Promise<TraceSummary | null> {
  for (let i = 0; i < 4; i++) {
    if (signal.aborted) return null;
    const [spans, logs] = await Promise.all([
      obleth.getRequestSpans(requestId).catch(() => []),
      obleth.usageLogs({ requestId, limit: 1 }).catch(() => []),
    ]);
    if (spans.length > 0 || logs.length > 0) return assembleTrace(logs[0] ?? null, spans);
    if (signal.aborted) return null;
    await new Promise((r) => setTimeout(r, 800));
  }
  return null;
}

export const testCapabilitiesTool: CharoTool<Args, CapabilityResult> = {
  name: "test_capabilities",
  description:
    "Probe a model's configured capabilities (quick ping, tool/web-search loop, JSON mode, vision) " +
    "by sending each as a real request through the gateway and reporting which boons fired.",
  parameters: {
    type: "object",
    properties: {
      model: { type: "string", description: "Model name (as configured) to test." },
      tests: { type: "array", items: { type: "string", enum: ALL_TESTS }, description: "Which capability tests to run." },
    },
    required: ["model"],
    additionalProperties: false,
  },
  resultType: "capability_result",
  requiresConfirmation: false,

  parseArgs(raw: unknown): Args {
    const o = (raw ?? {}) as Record<string, unknown>;
    const model = typeof o.model === "string" ? o.model.trim() : "";
    if (!model) throw new Error("test_capabilities requires a `model`.");
    const tests = Array.isArray(o.tests) && o.tests.length
      ? (o.tests as unknown[]).filter((t): t is TestId => ALL_TESTS.includes(t as TestId))
      : (["ping"] as TestId[]);
    const chosen = tests.length ? tests : (["ping"] as TestId[]);
    return { model, tests: [...new Set(chosen)] };
  },

  async run(args: Args, ctx: ToolCtx, emit: (p: ToolProgress) => void): Promise<CapabilityResult> {
    const tests: TestOutcome[] = [];
    for (const id of args.tests) {
      if (ctx.signal.aborted) break;
      const spec = TEST_CATALOG[id];
      const content = spec.needsImage
        ? [
            { type: "text" as const, text: spec.prompt },
            { type: "image_url" as const, image_url: { url: FIXTURE_IMAGE_DATA_URL } },
          ]
        : spec.prompt;
      const messages: ChatMessage[] = [{ role: "user", content }];

      let ok = false;
      let reply = "";
      let trace: TraceSummary | null = null;
      try {
        const res = await ctx.gatewayChat({ model: args.model, messages, stream: false }, ctx.signal);
        ok = res.ok;
        const requestId = res.headers.get("x-obleth-request-id");
        const body = await res.json().catch(() => null);
        reply = contentOf(body);
        if (requestId) trace = await fetchTrace(requestId, ctx.signal);
      } catch (e) {
        ok = false;
        reply = "";
      }

      const { status, detail } = evaluateTest(id, { ok, content: reply }, trace);
      const outcome: TestOutcome = { id, label: spec.label, status, detail, output: reply.trim().slice(0, 2000), trace };
      tests.push(outcome);
      emit({ kind: "capability_test", outcome });
    }
    return { modelName: args.model, tests };
  },
};
