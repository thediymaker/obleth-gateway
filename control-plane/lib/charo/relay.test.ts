import { describe, it, expect } from "vitest";
import { deltaText, deltaToolCalls, finishReason, readUpstreamSse } from "./relay";

const chunk = (choice: unknown) => ({ choices: [choice] });

describe("relay chunk parsing", () => {
  it("pulls token text", () => {
    expect(deltaText(chunk({ delta: { content: "hi" } }))).toBe("hi");
    expect(deltaText(chunk({ delta: {} }))).toBe("");
  });
  it("pulls tool_calls delta array", () => {
    expect(deltaToolCalls(chunk({ delta: { tool_calls: [{ index: 0 }] } }))).toEqual([{ index: 0 }]);
    expect(deltaToolCalls(chunk({ delta: {} }))).toBeUndefined();
  });
  it("reads finish_reason", () => {
    expect(finishReason(chunk({ finish_reason: "tool_calls" }))).toBe("tool_calls");
    expect(finishReason(chunk({}))).toBeUndefined();
  });
});

/** A response whose body yields `parts` one read at a time. */
function upstream(parts: string[]): Response {
  const enc = new TextEncoder();
  return new Response(new ReadableStream<Uint8Array>({
    start(c) {
      for (const p of parts) c.enqueue(enc.encode(p));
      c.close();
    },
  }));
}

async function read(parts: string[]): Promise<unknown[]> {
  const seen: unknown[] = [];
  await readUpstreamSse(upstream(parts), (c) => seen.push(c));
  return seen;
}

describe("readUpstreamSse", () => {
  const events = [{ n: 1 }, { n: 2 }];
  it("reads LF- and CRLF-framed events alike", async () => {
    expect(await read(['data: {"n":1}\n\ndata: {"n":2}\n\n'])).toEqual(events);
    expect(await read(['data: {"n":1}\r\n\r\ndata: {"n":2}\r\n\r\n'])).toEqual(events);
  });

  it("joins an event split across reads, even inside a CRLF", async () => {
    expect(await read(['data: {"n"', ':1}\r', '\n\r\ndata: {"n":2}\r\n', '\r\n'])).toEqual(events);
    expect(await read(['da', 'ta: {"n":1}\n', '\ndata: {"n":2}\n\n'])).toEqual(events);
  });

  it("stops at [DONE], skips keep-alives and comments, and reads an unterminated last event", async () => {
    expect(await read([': ping\n\ndata: {"n":1}\n\ndata: not json\n\ndata: [DONE]\n\ndata: {"n":3}\n\n'])).toEqual([{ n: 1 }]);
    expect(await read(['data: {"n":1}\n\ndata: {"n":2}'])).toEqual(events);
  });
});
