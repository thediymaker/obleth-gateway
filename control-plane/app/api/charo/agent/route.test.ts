import { beforeEach, expect, it, vi } from "vitest";
import { NextRequest } from "next/server";
const mocks = vi.hoisted(() => ({ guard: vi.fn(), chat: vi.fn() }));
vi.mock("@/lib/auth/guard", () => ({ guardAdmin: mocks.guard }));
vi.mock("@/lib/charo/gateway", () => ({ gatewayChat: mocks.chat }));
vi.mock("@/lib/obleth", () => ({ obleth: { getCharoSettings: async () => ({ brain_model: "brain" }) } }));
vi.mock("@/lib/charo/tools", () => ({ ensureToolsRegistered: () => {} }));
vi.mock("@/lib/charo/tools/registry", () => ({ toolSchemas: () => [], getTool: () => undefined }));
import { POST } from "./route";

const request = (body: unknown) => new NextRequest("http://localhost/api/charo/agent", { method: "POST", body: JSON.stringify(body) });
beforeEach(() => { vi.clearAllMocks(); mocks.guard.mockResolvedValue(null); });

it("denies non-admins before calling the brain", async () => {
  mocks.guard.mockResolvedValue(new Response("forbidden", { status: 403 }));
  expect((await POST(request({ messages: [] }))).status).toBe(403);
  expect(mocks.chat).not.toHaveBeenCalled();
});

it("streams the brain's CRLF-framed tokens as they arrive", async () => {
  const enc = new TextEncoder();
  let finish!: () => void;
  const upstream = new ReadableStream<Uint8Array>({
    start(c) {
      c.enqueue(enc.encode('data: {"choices":[{"delta":{"content":"Hel"}}]}\r\n\r\n'));
      finish = () => {
        c.enqueue(enc.encode('data: {"choices":[{"delta":{"content":"lo"},"finish_reason":"stop"}]}\r\n\r\ndata: [DONE]\r\n\r\n'));
        c.close();
      };
    },
  });
  mocks.chat.mockResolvedValue(new Response(upstream, { headers: { "Content-Type": "text/event-stream" } }));
  const res = await POST(request({ messages: [{ role: "user", content: "hi" }] }));
  const reader = res.body!.getReader();
  const dec = new TextDecoder();
  let seen = "";
  while (!seen.includes('"text":"Hel"')) seen += dec.decode((await reader.read()).value);
  finish();
  while (true) { const { value, done } = await reader.read(); if (done) break; seen += dec.decode(value); }
  expect(seen).toContain('event: token\ndata: {"text":"lo"}');
  expect(seen).toContain("event: done");
});
