import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ModelRoute } from "@/lib/obleth";
import { UnifiedWorkspace } from "./workspaces";

// Each answer turn renders its text through CharoMarkdown once per render, so
// counting its calls by the text's first word counts that turn's renders.
const renders = vi.hoisted(() => new Map<string, number>());
vi.mock("@/components/charo/markdown", () => ({
  CharoMarkdown: ({ text }: { text: string }) => {
    const turn = text.split(" ")[0];
    renders.set(turn, (renders.get(turn) ?? 0) + 1);
    return <p>{text}</p>;
  },
}));

const KEY = "obleth-playground:test:render-counts";
const models = [{ model_name: "alpha", model_type: "chat", enabled: true, context_window: 8192, input_cost_per_token: 0, output_cost_per_token: 0, tags: [] }] as unknown as ModelRoute[];
let root: Root;
let host: HTMLDivElement;

function renderWorkspace() {
  return act(async () => {
    root.render(
      <UnifiedWorkspace
        storageKey={KEY}
        session={{ id: "s1", title: "t", mode: "compare", models: ["alpha"], recipients: [0], generation: { systemPrompt: "" } }}
        update={() => {}}
        models={models}
        loading={false}
        settingsOpen={false}
        onOpenSession={() => {}}
        onPendingDone={() => {}}
      />,
    );
  });
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  localStorage.clear();
  renders.clear();
  localStorage.setItem(`${KEY}:0`, JSON.stringify({
    activeTarget: null, targetStart: null,
    messages: [
      { id: "u1", role: "user", content: "First question", promptId: "2:1:a" },
      { id: "a1", role: "assistant", content: "earlier answer" },
      { id: "u2", role: "user", content: "Second question", promptId: "2:2:b" },
      { id: "a2", role: "assistant", content: "previous answer" },
    ],
  }));
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

const textarea = () => host.querySelector<HTMLTextAreaElement>("[aria-label='Message']")!;
function type(value: string) {
  const setValue = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!;
  setValue.call(textarea(), value);
  textarea().dispatchEvent(new Event("input", { bubbles: true }));
}

describe("Playground render counts", () => {
  it("streaming 50 tokens into the last turn re-renders only that turn", async () => {
    let controller!: ReadableStreamDefaultController<Uint8Array>;
    const body = new ReadableStream<Uint8Array>({ start: (c) => { controller = c; } });
    vi.stubGlobal("fetch", vi.fn(async () => new Response(body, { headers: { "Content-Type": "text/event-stream" } })));
    await renderWorkspace();
    const before = { earlier: renders.get("earlier"), previous: renders.get("previous") };
    expect(before.earlier).toBeGreaterThan(0);

    await act(async () => type("Third question"));
    await act(async () => host.querySelector<HTMLButtonElement>("[aria-label='Send']")!.click());
    for (let i = 0; i < 50; i++) {
      await act(async () => {
        controller.enqueue(new TextEncoder().encode(`event: token\ndata: ${JSON.stringify({ text: i ? ` w${i}` : "streamed" })}\n\n`));
        await new Promise((r) => setTimeout(r, 0));
      });
    }
    await act(async () => { controller.close(); await new Promise((r) => setTimeout(r, 0)); });

    expect(renders.get("earlier")).toBe(before.earlier);
    expect(renders.get("previous")).toBe(before.previous);
    // Tokens are batched per animation frame, so the streaming turn renders
    // far less often than once a token, and ends with every token.
    expect(renders.get("streamed")).toBeGreaterThan(0);
    expect(renders.get("streamed")).toBeLessThan(50);
    expect(host.textContent).toContain("streamed w1 w2");
    expect(host.textContent).toContain(" w49");
  });

  it("typing in the composer re-renders no turn", async () => {
    await renderWorkspace();
    const before = new Map(renders);
    const draft = "A longer follow-up question";
    for (let i = 1; i <= draft.length; i++) await act(async () => type(draft.slice(0, i)));
    expect(textarea().value).toBe(draft);
    expect(new Map(renders)).toEqual(before);
  });
});
