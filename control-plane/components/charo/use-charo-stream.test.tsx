import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useCharoStream } from "./use-charo-stream";

// Token patches land once per animation frame. Frames never fire here, so
// every token a test sees arrived through a flush, not a frame.
let lane: ReturnType<typeof useCharoStream>;
let root: Root;
function Harness() {
  lane = useCharoStream({ model: "alpha", storageKey: "flush-test" });
  return null;
}
const tokens = (n: number) => Array.from({ length: n }, (_, i) => `event: token\ndata: ${JSON.stringify({ text: `t${i} ` })}\n\n`).join("");
const all = (n: number) => Array.from({ length: n }, (_, i) => `t${i} `).join("");
const encode = (text: string) => new TextEncoder().encode(text);

/** A response whose body the test writes to and closes. */
function openStream() {
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const body = new ReadableStream<Uint8Array>({ start: (c) => { controller = c; } });
  return { response: new Response(body, { headers: { "Content-Type": "text/event-stream" } }), controller };
}

beforeEach(async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  localStorage.clear();
  vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
  vi.stubGlobal("cancelAnimationFrame", vi.fn());
  root = createRoot(document.createElement("div"));
  await act(async () => { root.render(<Harness />); });
});
afterEach(async () => { await act(async () => root.unmount()); vi.unstubAllGlobals(); });

describe("token batching", () => {
  it("applies the last tokens when the stream ends", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(tokens(50), { headers: { "Content-Type": "text/event-stream" } })));
    await act(async () => { await lane.send("go"); });
    const turn = lane.messages.at(-1)!;
    expect(turn.content).toBe(all(50));
    expect(turn.streaming).toBe(false);
  });

  it("keeps the tokens shown so far when the stream is stopped", async () => {
    const { response, controller } = openStream();
    vi.stubGlobal("fetch", vi.fn(async () => response));
    await act(async () => { void lane.send("go"); });
    await act(async () => { controller.enqueue(encode(tokens(3))); await new Promise((r) => setTimeout(r, 0)); });
    await act(async () => { lane.stop(); });
    expect(lane.messages.at(-1)!.content).toBe(all(3));
  });

  it("keeps the tokens before an error", async () => {
    const body = tokens(2) + `event: error\ndata: ${JSON.stringify({ message: "upstream closed" })}\n\n`;
    vi.stubGlobal("fetch", vi.fn(async () => new Response(body, { headers: { "Content-Type": "text/event-stream" } })));
    await act(async () => { await lane.send("go"); });
    const turn = lane.messages.at(-1)!;
    expect(turn.content).toBe(all(2));
    expect(turn.error).toBe("upstream closed");
  });

  it("saves tokens still waiting for a frame when the lane unmounts", async () => {
    const { response, controller } = openStream();
    vi.stubGlobal("fetch", vi.fn(async () => response));
    await act(async () => { void lane.send("go"); });
    await act(async () => { controller.enqueue(encode(tokens(4))); await new Promise((r) => setTimeout(r, 0)); });
    await act(async () => root.render(null));
    const saved = JSON.parse(localStorage.getItem("flush-test")!) as { messages: { content: string }[] };
    expect(saved.messages.at(-1)!.content).toBe(all(4));
  });
});
