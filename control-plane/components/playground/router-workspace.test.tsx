import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RouterWorkspace } from "./router-workspace";
import { setAutoRouterSettingsAction } from "@/app/actions";
import type { PlaygroundSession } from "./playground";
import type { RouteExplainView, SimulateRouteRequest } from "@/lib/obleth";

vi.mock("@/app/actions", () => ({ setAutoRouterSettingsAction: vi.fn() }));

let root: Root;
let host: HTMLDivElement;
let calls: SimulateRouteRequest[];

// Mirrors the gateway: echoes any weight overrides in the request, falling
// back to the "saved" defaults (0.6 / 0.4 / 0.5 / 8 / 0 / false) when a field
// is omitted — exactly what an unweighted baseline call gets back.
function makeExplain(body: SimulateRouteRequest): RouteExplainView {
  return {
    chosen: body.prompt ? "model-a" : null,
    difficulty: 0.4,
    difficulty_source: "heuristic",
    tags: [],
    tag_source: "heuristic",
    classifier_ms: 0,
    tier_domains: [],
    tier_floor: 0,
    tier_floor_clamped: false,
    weights: {
      capacity: body.capacity_weight ?? 0.6,
      cost: body.cost_weight ?? 0.4,
      tag: body.tag_weight ?? 0.5,
      soft_cap: body.default_soft_cap ?? 8,
      difficulty_enabled: body.difficulty_enabled ?? false,
    },
    temperature: body.temperature ?? 0,
    uniform: body.uniform ?? 0,
    sampled: false,
    scored: [{ model: "model-a", level: 1, spare: 0.9, cost_score: 0.8, tag_score: 0.5, bias: 0, score: 0.7, chosen: true }],
    rejected: [],
  };
}

function setNativeValue(el: HTMLInputElement | HTMLTextAreaElement, value: string) {
  const descriptor = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), "value")!;
  descriptor.set!.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

const session: PlaygroundSession = {
  id: "s1", title: "Untitled session", mode: "router", models: ["charo"], generation: { systemPrompt: "" },
};

async function flush(ms = 350) {
  await act(async () => { await new Promise((r) => setTimeout(r, ms)); });
}

beforeEach(async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.mocked(setAutoRouterSettingsAction).mockReset();
  vi.mocked(setAutoRouterSettingsAction).mockResolvedValue({ ok: true });
  calls = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: string, init: RequestInit) => {
    const body = JSON.parse(init.body as string) as SimulateRouteRequest;
    calls.push(body);
    return new Response(JSON.stringify(makeExplain(body)), { status: 200, headers: { "Content-Type": "application/json" } });
  }));
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => { root.render(<RouterWorkspace session={session} update={() => {}} />); });
});

afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

describe("RouterWorkspace", () => {
  it("pins the same uniform draw across the baseline and edited calls of one run", async () => {
    const prompt = host.querySelector<HTMLTextAreaElement>('[aria-label="Prompt"]')!;
    await act(async () => setNativeValue(prompt, "Summarize this contract"));
    await flush();

    expect(calls.length).toBe(2);
    expect(typeof calls[0].uniform).toBe("number");
    expect(calls[0].uniform).toBe(calls[1].uniform);
    // One call carries no weight overrides (the baseline); the other does.
    expect(calls.some((c) => c.capacity_weight === undefined)).toBe(true);
    expect(calls.some((c) => c.capacity_weight !== undefined)).toBe(true);
  });

  it('reports "matches live" until a slider moves, then "✎ edited", and only writes settings on Apply', async () => {
    await flush(); // let the initial (empty-prompt) seed run settle
    expect(host.textContent).toContain("matches live");

    const applyButton = [...host.querySelectorAll("button")].find((b) => b.textContent?.includes("Apply to gateway")) as HTMLButtonElement;
    expect(applyButton.disabled).toBe(true);
    expect(setAutoRouterSettingsAction).not.toHaveBeenCalled();

    const slider = host.querySelector<HTMLInputElement>("#router-capacity-weight")!;
    await act(async () => setNativeValue(slider, "0.95"));
    expect(host.textContent).toContain("✎ edited");
    expect(applyButton.disabled).toBe(false);
    expect(setAutoRouterSettingsAction).not.toHaveBeenCalled();

    await act(async () => {
      applyButton.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      await new Promise((r) => setTimeout(r, 50));
    });
    expect(setAutoRouterSettingsAction).toHaveBeenCalledTimes(1);
    expect(vi.mocked(setAutoRouterSettingsAction).mock.calls[0][0].capacity_weight).toBeCloseTo(0.95);
  });

  it("debounces slider drags instead of firing a request per change", async () => {
    await flush();
    calls = [];
    const slider = host.querySelector<HTMLInputElement>("#router-cost-weight")!;
    await act(async () => {
      setNativeValue(slider, "0.1");
      setNativeValue(slider, "0.2");
      setNativeValue(slider, "0.3");
    });
    expect(calls.length).toBe(0); // nothing fired yet — still inside the debounce window
    await flush();
    expect(calls.length).toBe(2); // exactly one run (baseline + edited) for the whole drag
  });
});
