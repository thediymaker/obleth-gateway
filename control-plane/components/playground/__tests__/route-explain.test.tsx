import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { RouteExplainPanel } from "../route-explain";
import type { RouteExplainView } from "@/lib/obleth";

let root: Root;
let host: HTMLDivElement;

// Every `score` below is *forward*-computed from the gateway's own formula
// (obleth-config's routing/mod.rs:497-528), not back-solved from what the
// renderer happens to print:
//
//   base  = capacity * spare + cost * cost_score
//   blend = tags.is_empty() ? base : tag * tag_score + (1 - tag) * base
//   score = blend * bias.clamp(0.1, 3.0)
//
// The untagged fixture below carries bias = 1.0 (the router's own default,
// and a value within the reachable [0.1, 3.0] clamp range — 0 is not).

// base  = 0.6*0.9 + 0.4*0.7           = 0.82
// blend = base (no requested tags)    = 0.82
// score = 0.82 * 1.0                  = 0.82
const chosenUntagged = { model: "gpt-oss-120b", level: 2, spare: 0.9, cost_score: 0.7, tag_score: 0, bias: 1.0, score: 0.82, chosen: true };
// base  = 0.6*0.3 + 0.4*0.5 = 0.38; untagged -> blend = base = 0.38; score = 0.38 * 1.0 = 0.38
const otherUntagged = { model: "small-model", level: 1, spare: 0.3, cost_score: 0.5, tag_score: 0, bias: 1.0, score: 0.38, chosen: false };

const untagged: RouteExplainView = {
  chosen: "gpt-oss-120b",
  difficulty: 3,
  difficulty_source: "heuristic",
  tags: [],
  tag_source: "heuristic",
  classifier_ms: 0,
  tier_domains: [],
  tier_floor: 0,
  tier_floor_clamped: false,
  weights: { capacity: 0.6, cost: 0.4, tag: 0.5, soft_cap: 8, difficulty_enabled: false },
  temperature: 0,
  uniform: 0.5,
  sampled: false,
  scored: [chosenUntagged, otherUntagged],
  rejected: [{ reason: "context window too small", models: ["tiny-model"] }],
};

// A non-neutral bias (1.3, plausible under the [0.1, 3.0] clamp) and a
// non-empty `tags` so the blend branch and the bias *multiplier* are both
// pinned — this is the exact combination review round 1 found unverified:
//   base  = 0.6*0.8 + 0.4*0.6                     = 0.72
//   blend = 0.5*1.0 (tag_score) + 0.5*0.72 (base) = 0.86
//   score = 0.86 * 1.3                            = 1.118
const taggedRow = { model: "gpt-oss-120b", level: 2, spare: 0.8, cost_score: 0.6, tag_score: 1.0, bias: 1.3, score: 1.118, chosen: true };
const tagged: RouteExplainView = { ...untagged, tags: ["coding"], scored: [taggedRow] };

// For the baseline/edited comparison: same shape as `untagged`, but a
// different candidate wins (used only to check the "was chosen" marking
// logic, not the arithmetic itself).
const edited: RouteExplainView = {
  ...untagged,
  chosen: "small-model",
  scored: [
    { ...chosenUntagged, chosen: false },
    { model: "small-model", level: 1, spare: 0.95, cost_score: 0.9, tag_score: 0, bias: 1.0, score: 0.93, chosen: true },
  ],
};

async function render(explain: RouteExplainView, baseline?: RouteExplainView) {
  await act(async () => { root.render(<RouteExplainPanel explain={explain} baseline={baseline} />); });
}

function clickModel(name: string) {
  const button = [...host.querySelectorAll("button")].find((b) => b.textContent?.includes(name))!;
  button.dispatchEvent(new MouseEvent("click", { bubbles: true }));
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

describe("RouteExplainPanel", () => {
  it("shows the untagged branch's base-only arithmetic with the numbers substituted", async () => {
    await render(untagged);
    await act(async () => clickModel("gpt-oss-120b"));
    expect(host.textContent).toMatch(/spare/i);
    expect(host.textContent).toMatch(/no requested tags/i);
    expect(host.textContent).toContain("0.820"); // base === blend === score at bias 1.0
  });

  it("shows the tagged branch's blend and the bias multiplier, not an addend", async () => {
    await render(tagged);
    await act(async () => clickModel("gpt-oss-120b"));
    expect(host.textContent).toContain("0.720"); // base
    expect(host.textContent).toContain("0.860"); // blend
    expect(host.textContent).toContain("1.118"); // score = blend * bias, not blend + bias
  });

  it("collapses rejections under their reason", async () => {
    await render(untagged);
    expect(host.textContent).toMatch(/context window/i);
  });

  it("warns when the tier floor was clamped down", async () => {
    await render({ ...untagged, tier_floor_clamped: true });
    const status = host.querySelector('[role="status"]');
    expect(status).not.toBeNull();
    expect(status!.textContent).toMatch(/strongest.*unavailable/i);
  });

  it("shows no clamp warning when the tier floor was not clamped", async () => {
    await render({ ...untagged, tier_floor_clamped: false });
    expect(host.querySelector('[role="status"]')).toBeNull();
  });

  it("shows each survivor's tier level against the floor when tiering is on", async () => {
    await render({
      ...untagged,
      weights: { ...untagged.weights, difficulty_enabled: true },
      tier_domains: ["coding"],
      tier_floor: 1,
    });
    expect(host.textContent).toContain("tier 2");
    await act(async () => clickModel("gpt-oss-120b"));
    expect(host.textContent).toMatch(/tier level.*in coding.*floor 1.*cleared the floor/i);
  });

  it("renders the synthetic all-models domain as prose, not the sentinel", async () => {
    await render({
      ...untagged,
      weights: { ...untagged.weights, difficulty_enabled: true },
      tier_domains: ["*"],
      tier_floor: 1,
    });
    expect(host.textContent).toContain("all models");
    expect(host.textContent).not.toContain("Domains *");
  });

  it("says nothing about tiers when the tier stage did not run", async () => {
    await render(untagged); // difficulty_enabled: false
    expect(host.textContent).not.toMatch(/tier 2/i);
    await act(async () => clickModel("gpt-oss-120b"));
    expect(host.textContent).not.toMatch(/tier level/i);
  });

  it("marks the model the live weights would have chosen", async () => {
    await render(edited, untagged);
    expect(host.textContent).toMatch(/was chosen/i);
  });
});
