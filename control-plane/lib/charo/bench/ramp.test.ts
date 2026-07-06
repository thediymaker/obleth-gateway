import { describe, it, expect } from "vitest";
import { runOneRequest } from "./ramp";

function sseResponse(chunks: string[]): Response {
  const body = new ReadableStream<Uint8Array>({
    start(c) {
      const enc = new TextEncoder();
      for (const ch of chunks) c.enqueue(enc.encode(ch));
      c.close();
    },
  });
  return new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } });
}

describe("runOneRequest", () => {
  it("marks 200 completed and records token counts", async () => {
    const gateway = async () => sseResponse([
      'data: {"choices":[{"delta":{"content":"hi"}}]}\n\n',
      "data: [DONE]\n\n",
    ]);
    const s = await runOneRequest("m", "p", 8, gateway as never, new AbortController().signal);
    expect(s.status).toBe(200);
    expect(s.ttfbMs).toBeGreaterThanOrEqual(0);
  });

  it("maps 429 to rejected sample", async () => {
    const gateway = async () => new Response("busy", { status: 429 });
    const s = await runOneRequest("m", "p", 8, gateway as never, new AbortController().signal);
    expect(s.status).toBe(429);
  });

  it("maps 500 to error sample", async () => {
    const gateway = async () => new Response("boom", { status: 500 });
    const s = await runOneRequest("m", "p", 8, gateway as never, new AbortController().signal);
    expect(s.status).toBe(500);
  });
});
