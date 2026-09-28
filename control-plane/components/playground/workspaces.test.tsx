import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ModelRoute } from "@/lib/obleth";
import type { PlaygroundSession } from "./playground";
import { UnifiedWorkspace } from "./workspaces";

let root: Root;
let host: HTMLDivElement;
let latest: PlaygroundSession;

const KEY = "obleth-playground:test:s1:compare";
const models = [
  { model_name: "alpha", model_type: "chat", enabled: true, context_window: 8192, input_cost_per_token: 1e-6, output_cost_per_token: 2e-6, tags: [] },
  { model_name: "beta", model_type: "chat", enabled: true, context_window: 8192, input_cost_per_token: 1e-6, output_cost_per_token: 2e-6, tags: [] },
  { model_name: "gamma", model_type: "chat", enabled: true, context_window: 8192, input_cost_per_token: 1e-6, output_cost_per_token: 2e-6, tags: [] },
] as unknown as ModelRoute[];

function lane(i: number, answer: string, metrics: { inputTokens: number; outputTokens: number; ttftMs: number; totalMs: number }) {
  localStorage.setItem(`${KEY}:${i}`, JSON.stringify({
    activeTarget: null, targetStart: null,
    messages: [
      { id: `u${i}`, role: "user", content: "Which is faster?", promptId: "2:1:p" },
      { id: `a${i}`, role: "assistant", content: answer, metrics, requestId: `req-${i}` },
    ],
  }));
}

function Harness({ initial }: { initial: PlaygroundSession }) {
  const [session, setSession] = useState(initial);
  latest = session;
  return (
    <UnifiedWorkspace
      storageKey={KEY}
      session={session}
      update={(patch) => setSession((s) => ({ ...s, ...patch }))}
      models={models}
      loading={false}
      settingsOpen={false}
      onOpenSession={() => {}}
      onPendingDone={() => {}}
    />
  );
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  localStorage.clear();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  vi.stubGlobal("fetch", vi.fn(async () => new Response("{}", { status: 200 })));
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});

const card = (name: string) => host.querySelector<HTMLElement>(`[aria-label='${name} response']`)!;

describe("UnifiedWorkspace", () => {
  it("shows one prompt over side-by-side answers, each with its numbers and the axes it won", async () => {
    lane(0, "alpha says", { inputTokens: 10, outputTokens: 50, ttftMs: 300, totalMs: 1300 });
    lane(1, "beta says", { inputTokens: 10, outputTokens: 20, ttftMs: 900, totalMs: 1900 });
    await act(async () => {
      root.render(<Harness initial={{ id: "s1", title: "t", mode: "compare", models: ["alpha", "beta"], recipients: [0, 1], generation: { systemPrompt: "" } }} />);
    });

    expect(host.textContent).toContain("You · sent to 2 models");
    expect(host.textContent?.match(/Which is faster\?/g)).toHaveLength(1);
    expect(card("alpha").textContent).toContain("300ms to first token");
    expect(card("alpha").textContent).toContain("50 tok/s"); // 50 tokens over the 1 s after the first
    expect(card("alpha").textContent).toContain("Fastest first token");
    expect(card("beta").textContent).toContain("Fewest tokens");
    expect(card("beta").textContent).toContain("Lowest cost");
    expect(card("beta").querySelector("a[href='/logs?requestId=req-1']")).not.toBeNull();
  });

  it("toggles who receives the next message from the Send to row", async () => {
    await act(async () => {
      root.render(<Harness initial={{ id: "s1", title: "t", mode: "compare", models: ["alpha", "beta"], recipients: [0, 1], generation: { systemPrompt: "" } }} />);
    });
    const beta = [...host.querySelectorAll("button[aria-pressed]")].find((b) => b.textContent === "beta") as HTMLButtonElement;
    await act(async () => beta.click());
    expect(latest.recipients).toEqual([0]);
    expect(host.querySelector<HTMLTextAreaElement>("[aria-label='Message']")!.placeholder).toBe("Message 1 model…");
  });

  it("reuses an empty lane for a newly picked model and keeps lanes that have history", async () => {
    lane(0, "alpha says", { inputTokens: 10, outputTokens: 50, ttftMs: 300, totalMs: 1300 });
    await act(async () => {
      root.render(<Harness initial={{ id: "s1", title: "t", mode: "compare", models: ["alpha", "beta"], recipients: [0], generation: { systemPrompt: "" } }} />);
    });
    await act(async () => [...host.querySelectorAll("button")].find((b) => b.textContent === "Add model")!.click());
    const tick = (name: string) => document.querySelector<HTMLButtonElement>(`[aria-label='Add ${name} to comparison']`)!;
    await act(async () => { tick("beta").click(); tick("gamma").click(); });
    await act(async () => [...document.querySelectorAll("button")].find((b) => b.textContent?.startsWith("Compare 3 models"))!.click());
    // alpha keeps lane 0 (it has history), beta keeps lane 1, gamma is appended.
    expect(latest.models).toEqual(["alpha", "beta", "gamma"]);
    expect(latest.recipients).toEqual([0, 1, 2]);
  });
});
