import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { RouteExplainPanel } from "../route-explain";
import type { RouteExplainView } from "@/lib/obleth";

let root: Root;
let host: HTMLDivElement;

// Chosen candidate's arithmetic is picked so capacity*spare + cost*cost_score
// + tag*tag_score + bias sums to exactly `score` (0.812), the value the
// expanded row is expected to render.
const fixture: RouteExplainView = {
  chosen: "gpt-oss-120b",
  difficulty: 0.62,
  difficulty_source: "heuristic",
  tags: ["coding"],
  tag_source: "header",
  classifier_ms: 0,
  tier_domains: ["coding"],
  tier_floor: 2,
  tier_floor_clamped: false,
  weights: { capacity: 0.6, cost: 0.1, tag: 0.4, soft_cap: 8, difficulty_enabled: false },
  temperature: 0,
  uniform: 0.5,
  sampled: false,
  scored: [
    { model: "gpt-oss-120b", level: 2, spare: 0.9, cost_score: 0.72, tag_score: 0.5, bias: 0, score: 0.812, chosen: true },
    { model: "small-model", level: 1, spare: 0.5, cost_score: 0.6, tag_score: 0.2, bias: 0, score: 0.34, chosen: false },
  ],
  rejected: [{ reason: "context window too small", models: ["tiny-model"] }],
};

const edited: RouteExplainView = {
  ...fixture,
  chosen: "small-model",
  scored: [
    { model: "gpt-oss-120b", level: 2, spare: 0.9, cost_score: 0.72, tag_score: 0.5, bias: 0, score: 0.5, chosen: false },
    { model: "small-model", level: 1, spare: 0.95, cost_score: 0.8, tag_score: 0.9, bias: 0, score: 0.9, chosen: true },
  ],
};

async function render(explain: RouteExplainView, baseline?: RouteExplainView) {
  await act(async () => { root.render(<RouteExplainPanel explain={explain} baseline={baseline} />); });
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

describe("RouteExplainPanel", () => {
  it("shows the arithmetic behind a scored candidate once expanded", async () => {
    await render(fixture);
    const button = [...host.querySelectorAll("button")].find((b) => b.textContent?.includes("gpt-oss-120b"))!;
    await act(async () => { button.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    expect(host.textContent).toMatch(/spare/i);
    expect(host.textContent).toContain("0.812");
  });

  it("collapses rejections under their reason", async () => {
    await render(fixture);
    expect(host.textContent).toMatch(/context window/i);
  });

  it("warns when the tier floor was clamped down", async () => {
    await render({ ...fixture, tier_floor_clamped: true });
    const status = host.querySelector('[role="status"]');
    expect(status).not.toBeNull();
    expect(status!.textContent).toMatch(/strongest.*unavailable/i);
  });

  it("shows no clamp warning when the tier floor was not clamped", async () => {
    await render({ ...fixture, tier_floor_clamped: false });
    expect(host.querySelector('[role="status"]')).toBeNull();
  });

  it("marks the model the live weights would have chosen", async () => {
    await render(edited, fixture);
    expect(host.textContent).toMatch(/was chosen/i);
  });
});
