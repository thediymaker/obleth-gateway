import type { TraceSummary } from "@/lib/charo/trace";
import type { TestId, TestStatus } from "./types";

// A 1x1-ish solid-colour PNG is enough to exercise the vision path without a
// large payload; we only assert the boon fired and returned *something*, not
// that the description is accurate.
export const FIXTURE_IMAGE_DATA_URL =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAYAAADED76LAAAAFklEQVR4nGP8z8BQz0AEYBpVSF+FANlgAxlF0RcYAAAAAElFTkSuQmCC";

export const TEST_CATALOG: Record<TestId, { label: string; prompt: string; needsImage?: boolean }> = {
  ping: {
    label: "Quick ping",
    prompt: "Reply with a short sentence to confirm you're responding.",
  },
  tools: {
    label: "Tools / web search",
    prompt:
      "Search the web for a surprising fact about octopuses and summarise what you find, citing your source.",
  },
  json: {
    label: "Force JSON",
    prompt: 'Reply with ONLY this JSON object and nothing else: {"status":"ok","gateway":"obleth"}',
  },
  vision: {
    label: "Describe image",
    prompt: "Describe the attached image in a sentence.",
    needsImage: true,
  },
};

function parseableJson(s: string): boolean {
  const t = s.trim();
  if (!t) return false;
  // Tolerate a ```json fence around the object.
  const stripped = t.replace(/^```(?:json)?\s*/i, "").replace(/\s*```$/, "").trim();
  try { JSON.parse(stripped); return true; } catch { return false; }
}

function toolFired(trace: TraceSummary | null): boolean {
  return !!trace && (trace.toolLoopIters > 0 || trace.boonsFired.includes("tool_loop") || trace.toolsCalled.length > 0);
}

/** Pure PASS/WARN/FAIL evaluation for one test, given the reply and its trace. */
export function evaluateTest(
  id: TestId,
  res: { ok: boolean; content: string },
  trace: TraceSummary | null,
): { status: TestStatus; detail: string } {
  const content = res.content.trim();
  const visionErr = !!trace && trace.errorStages.some((s) => s.startsWith("boon:vision"));
  if (!res.ok) return { status: "fail", detail: `request failed${trace ? ` (status ${trace.statusCode})` : ""}` };

  switch (id) {
    case "ping":
      return content
        ? { status: "pass", detail: "responded" }
        : { status: "fail", detail: "empty reply" };
    case "json":
      return parseableJson(content)
        ? { status: "pass", detail: "valid JSON" }
        : { status: "fail", detail: "reply was not valid JSON" };
    case "tools":
      if (toolFired(trace)) {
        const names = trace!.toolsCalled.join(", ");
        return { status: "pass", detail: names ? `tool_loop fired (${names})` : "tool_loop fired" };
      }
      if (!content) return { status: "fail", detail: "empty reply" };
      return {
        status: "warn",
        detail: trace ? "answered, but no tool call was recorded" : "answered; tool trace unavailable",
      };
    case "vision":
      if (visionErr) return { status: "fail", detail: "vision boon errored" };
      if (!content) return { status: "fail", detail: "empty reply" };
      return trace?.boonsFired.includes("vision")
        ? { status: "pass", detail: "vision boon fired" }
        : { status: "pass", detail: "described the image" };
  }
}
