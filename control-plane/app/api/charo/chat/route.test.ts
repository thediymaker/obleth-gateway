import { beforeEach, expect, it, vi } from "vitest";
import { NextRequest } from "next/server";
const mocks = vi.hoisted(() => ({ guard: vi.fn(), chat: vi.fn(), images: vi.fn() }));
vi.mock("@/lib/auth/guard", () => ({ guardAdmin: mocks.guard }));
vi.mock("@/lib/charo/gateway", () => ({ gatewayChat: mocks.chat, gatewayImages: mocks.images }));
vi.mock("@/lib/obleth", () => ({ obleth: { listModels: async () => [], getRequestSpans: async () => [], usageLogs: async () => [] } }));
import { POST } from "./route";
const request = (body: unknown) => new NextRequest("http://localhost/api/charo/chat", { method: "POST", body: JSON.stringify(body) });
beforeEach(() => { vi.clearAllMocks(); mocks.guard.mockResolvedValue(null); });
it("denies unauthorized callers before using the system key", async () => {
  mocks.guard.mockResolvedValue(new Response("unauthorized", { status: 401 }));
  expect((await POST(request({}))).status).toBe(401); expect(mocks.chat).not.toHaveBeenCalled();
});
it("validates input before invoking inference", async () => {
  expect((await POST(request({ model: "m", messages: [{ role: "user", content: "hi" }], generation: { maxTokens: -2 } }))).status).toBe(400);
  expect(mocks.chat).not.toHaveBeenCalled();
});
it("passes comparison parameters without the assistant persona and returns measured usage", async () => {
  mocks.chat.mockResolvedValue(new Response('data: {"choices":[{"delta":{"content":"answer"}}]}\r\n\r\ndata: {"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":4}}\r\n\r\ndata: [DONE]\r\n\r\n', { headers: { "Content-Type": "text/event-stream", "x-obleth-request-id": "request-1" } }));
  const response = await POST(request({ model: "m", bare: true, messages: [{ role: "user", content: "hi" }], generation: { systemPrompt: "Be brief", temperature: 0.2, maxTokens: 50 } }));
  const text = await response.text();
  expect(mocks.chat.mock.calls[0][0]).toMatchObject({ messages: [{ role: "system", content: "Be brief" }, { role: "user", content: "hi" }], temperature: 0.2, max_tokens: 50 });
  expect(text).toContain('"outputTokens":4'); expect(text).toContain('"requestId":"request-1"'); expect(text).toContain('"text":"answer"');
});
