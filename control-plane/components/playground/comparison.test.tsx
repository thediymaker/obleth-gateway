import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useCharoStream } from "@/components/charo/use-charo-stream";

type Stream = ReturnType<typeof useCharoStream>;
let lanes: Stream[];
let root: Root;
let host: HTMLDivElement;
function Harness() {
  lanes = [useCharoStream({ model: "alpha", storageKey: "test-alpha" }), useCharoStream({ model: "beta", storageKey: "test-beta" })];
  return null;
}
const response = (text: string) => new Response(`event: token\ndata: ${JSON.stringify({ text })}\n\nevent: done\ndata: {}\n\n`, { headers: { "Content-Type": "text/event-stream" } });
beforeEach(async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  localStorage.clear(); host = document.createElement("div"); root = createRoot(host);
  await act(async () => { root.render(<Harness />); });
});
afterEach(async () => { await act(async () => root.unmount()); vi.unstubAllGlobals(); });

describe("comparison conversations", () => {
  it("broadcasts a shared prompt but retains each model's own answer for follow-ups and retries", async () => {
    const calls: Array<{ model: string; messages: { content: string }[] }> = [];
    vi.stubGlobal("fetch", vi.fn(async (_url, init) => { const body = JSON.parse(init.body); calls.push(body); return response(`${body.model} answer`); }));
    await act(async () => { await Promise.all(lanes.map((lane) => lane.send("first"))); });
    await act(async () => { await Promise.all(lanes.map((lane) => lane.send("follow up"))); });
    expect(calls[2].messages.map((m) => m.content)).toEqual(["first", "alpha answer", "follow up"]);
    expect(calls[3].messages.map((m) => m.content)).toEqual(["first", "beta answer", "follow up"]);
    await act(async () => { await lanes[0].retry(); });
    expect(calls[4].messages.map((m) => m.content)).toEqual(["first", "alpha answer", "follow up"]);
    expect(lanes[1].messages).toHaveLength(4);
  });

  it("keeps a peer's result when one model fails", async () => {
    vi.stubGlobal("fetch", vi.fn(async (_url, init) => JSON.parse(init.body).model === "alpha" ? new Response("unavailable", { status: 503 }) : response("beta succeeds")));
    await act(async () => { await Promise.all(lanes.map((lane) => lane.send("hello"))); });
    expect(lanes[0].messages.at(-1)?.error).toContain("503");
    expect(lanes[1].messages.at(-1)?.content).toBe("beta succeeds");
    expect(lanes.every((lane) => !lane.busy)).toBe(true);
  });

  it("stops one pending request without aborting its peer; unmount cancels remaining work", async () => {
    const signals: AbortSignal[] = [];
    vi.stubGlobal("fetch", vi.fn((_url, init) => new Promise((_resolve, reject) => {
      signals.push(init.signal); init.signal.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")));
    })));
    await act(async () => { lanes.forEach((lane) => void lane.send("wait")); });
    await act(async () => { lanes[0].stop(); });
    expect(signals[0].aborted).toBe(true); expect(signals[1].aborted).toBe(false);
    expect(lanes[1].busy).toBe(true);
    await act(async () => root.render(null));
    expect(signals[1].aborted).toBe(true);
  });

  it("restores saved lane history without launching requests", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => response("saved answer")));
    await act(async () => { await lanes[0].send("remember me"); });
    await act(async () => root.render(null));
    const fetch = vi.fn(); vi.stubGlobal("fetch", fetch);
    await act(async () => root.render(<Harness />));
    expect(lanes[0].messages.map((m) => m.content)).toEqual(["remember me", "saved answer"]);
    expect(lanes[0].busy).toBe(false); expect(fetch).not.toHaveBeenCalled();
  });
});
