import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { SpanExpandDetail, type SpanNode } from "./request-detail";
import type { SpanEntry } from "@/lib/obleth";

// Forward-computed from the gateway's own formula, same as Task 13's fixture:
// base = 0.6*0.9 + 0.4*0.7 = 0.82; untagged -> blend = base; score = base * 1.0
const scoredRow = {
  model: "gpt-oss-120b",
  level: 2,
  spare: 0.9,
  cost_score: 0.7,
  tag_score: 0,
  bias: 1.0,
  score: 0.82,
  chosen: true,
};

const newShapeExplain = {
  chosen: "gpt-oss-120b",
  difficulty: 3,
  difficulty_source: "heuristic",
  tags: [],
  tag_source: "heuristic",
  classifier_ms: 4,
  tier_domains: [],
  tier_floor: 0,
  tier_floor_clamped: false,
  weights: { capacity: 0.6, cost: 0.4, tag: 0.5, soft_cap: 8, difficulty_enabled: false },
  temperature: 0,
  uniform: 0.5,
  sampled: false,
  scored: [scoredRow],
  rejected: [{ reason: "context window too small", models: ["tiny-model"] }],
};

function span(over: Partial<SpanEntry> = {}): SpanEntry {
  return {
    request_id: "req-1",
    span_name: "auto_route",
    parent_span: "proxy_request",
    start_ms: 0,
    duration_ms: 5,
    status: "ok",
    attributes: "{}",
    ...over,
  };
}

function node(over: Partial<SpanEntry> = {}): SpanNode {
  return { span: span(over), children: [] };
}

let root: Root;
let host: HTMLDivElement;

async function render(n: SpanNode) {
  await act(async () => {
    root.render(<SpanExpandDetail node={n} onClose={() => {}} />);
  });
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

describe("auto_route span rendering", () => {
  it("renders RouteExplainPanel for a new-shape span (scored array present)", async () => {
    await render(node({ attributes: JSON.stringify(newShapeExplain) }));
    // Only RouteExplainPanel produces per-candidate score rows and the
    // rejection grouping by reason — assert on both.
    expect(host.textContent).toContain("gpt-oss-120b");
    expect(host.textContent).toContain("0.820");
    expect(host.textContent).toMatch(/context window too small/i);
    expect(host.textContent).toMatch(/not considered/i);
  });

  it("falls back to raw attribute rendering for a pre-upgrade span (regression guard)", async () => {
    const legacy = { chosen: "m", candidates: 7, tags: ["coding"] };
    await render(node({ attributes: JSON.stringify(legacy) }));
    // Raw dl rendering shows each key/value pair, including the field name
    // that a truthiness check on `candidates` (a number, not an array)
    // would otherwise misclassify as the new shape.
    expect(host.textContent).toContain("candidates");
    expect(host.textContent).toContain("7");
    expect(host.textContent).toContain("chosen");
    // The panel never rendered: no "Not considered" rejection grouping.
    expect(host.textContent).not.toMatch(/not considered/i);
  });

  it("falls back to raw rendering for the gateway's serialization-failure payload", async () => {
    const failure = { chosen: "m", error: "serde_json::to_value failed" };
    await render(node({ attributes: JSON.stringify(failure) }));
    expect(host.textContent).toContain("error");
    expect(host.textContent).toMatch(/serialization-failure|serde_json/i);
  });

  it("does not crash on unparseable attributes", async () => {
    await render(node({ attributes: "not json {{{" }));
    expect(host.querySelector(".rounded-md")).not.toBeNull();
  });

  it("does not crash on empty attributes", async () => {
    await render(node({ attributes: "{}" }));
    expect(host.querySelector(".rounded-md")).not.toBeNull();
  });

  it("leaves non-auto_route spans on the raw renderer even with a scored-shaped payload", async () => {
    await render(node({ span_name: "upstream", attributes: JSON.stringify(newShapeExplain) }));
    expect(host.textContent).not.toMatch(/not considered/i);
  });
});
