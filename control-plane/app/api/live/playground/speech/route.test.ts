import { beforeEach, describe, expect, it, vi } from "vitest";
import { NextRequest } from "next/server";
const mocks = vi.hoisted(() => ({ guard: vi.fn(), speech: vi.fn() }));
vi.mock("@/lib/auth/roles", () => ({ requireAdmin: mocks.guard }));
vi.mock("@/lib/charo/gateway", () => ({ gatewaySpeech: mocks.speech }));
import { POST } from "./route";

const request = (body: unknown) =>
  new NextRequest("http://localhost/api/live/playground/speech", { method: "POST", body: JSON.stringify(body) });

beforeEach(() => {
  vi.clearAllMocks();
  mocks.guard.mockResolvedValue({ email: "admin@example.edu" });
});

describe("POST /api/live/playground/speech", () => {
  it("denies unauthorized callers before using the system key", async () => {
    mocks.guard.mockRejectedValue(new Error("not an admin"));
    expect((await POST(request({ model: "tts", input: "hi" }))).status).toBe(401);
    expect(mocks.speech).not.toHaveBeenCalled();
  });

  it.each([
    ["no text", { model: "tts", input: "" }],
    ["only spaces", { model: "tts", input: "   " }],
    ["no model", { input: "hi" }],
    ["text over the cap", { model: "tts", input: "x".repeat(20_001) }],
    ["a voice over the cap", { model: "tts", input: "hi", voice: "v".repeat(201) }],
    ["instructions over the cap", { model: "tts", input: "hi", instructions: "i".repeat(2001) }],
  ])("rejects %s before calling the gateway", async (_label, body) => {
    const res = await POST(request(body));
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "Invalid speech request." });
    expect(mocks.speech).not.toHaveBeenCalled();
  });

  it("asks for streamed WAV, trims the options and hands over the request's abort signal", async () => {
    mocks.speech.mockResolvedValue(new Response(new Uint8Array([1]), { status: 200 }));
    const req = request({ model: "tts", input: "Hello there.", voice: " tara ", instructions: " calm and slow " });
    await POST(req);
    expect(mocks.speech).toHaveBeenCalledTimes(1);
    expect(mocks.speech.mock.calls[0][0]).toEqual({
      model: "tts",
      input: "Hello there.",
      voice: "tara",
      instructions: "calm and slow",
      response_format: "wav",
      stream_format: "audio",
    });
    expect(mocks.speech.mock.calls[0][1]).toBe(req.signal);
  });

  it("leaves out a blank voice and blank instructions so the server uses its defaults", async () => {
    mocks.speech.mockResolvedValue(new Response(new Uint8Array([1]), { status: 200 }));
    await POST(request({ model: "tts", input: "hi", voice: "", instructions: "  " }));
    expect(mocks.speech.mock.calls[0][0]).toEqual({ model: "tts", input: "hi", response_format: "wav", stream_format: "audio" });
  });

  it("passes the gateway's error message and status through", async () => {
    mocks.speech.mockResolvedValue(new Response(JSON.stringify({ error: { message: "voice 'zed' not found" } }), {
      status: 400,
      headers: { "content-type": "application/json", "x-obleth-request-id": "req-9" },
    }));
    const res = await POST(request({ model: "tts", input: "hi", voice: "zed" }));
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "voice 'zed' not found", requestId: "req-9" });
  });

  it("falls back to a plain message when the error body is not JSON", async () => {
    mocks.speech.mockResolvedValue(new Response("<html>Bad Gateway</html>", { status: 502 }));
    const res = await POST(request({ model: "tts", input: "hi" }));
    expect(res.status).toBe(502);
    expect((await res.json()).error).toBe("speech failed (HTTP 502)");
  });

  it("reports an unreachable gateway as a 502", async () => {
    mocks.speech.mockRejectedValue(new TypeError("fetch failed", { cause: new Error("connect ECONNREFUSED") }));
    const res = await POST(request({ model: "tts", input: "hi" }));
    expect(res.status).toBe(502);
    expect(await res.json()).toEqual({ error: "The gateway could not be reached." });
  });

  it("streams the audio through as it arrives, with headers that stop buffering", async () => {
    let push!: ReadableStreamDefaultController<Uint8Array>;
    const upstream = new ReadableStream<Uint8Array>({ start(c) { push = c; } });
    mocks.speech.mockResolvedValue(new Response(upstream, {
      status: 200,
      headers: { "content-type": "audio/wav", "x-obleth-request-id": "req-1", "content-length": "999" },
    }));
    const res = await POST(request({ model: "tts", input: "hi" }));
    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toBe("audio/wav");
    expect(res.headers.get("cache-control")).toBe("no-store");
    expect(res.headers.get("x-accel-buffering")).toBe("no");
    expect(res.headers.get("x-obleth-request-id")).toBe("req-1");
    expect(res.headers.get("content-length")).toBeNull();

    // The first bytes are readable before the upstream has finished.
    const reader = res.body!.getReader();
    push.enqueue(new Uint8Array([82, 73, 70, 70]));
    expect((await reader.read()).value).toEqual(new Uint8Array([82, 73, 70, 70]));
    push.enqueue(new Uint8Array([1, 2]));
    push.close();
    expect((await reader.read()).value).toEqual(new Uint8Array([1, 2]));
    expect((await reader.read()).done).toBe(true);
  });

  it("labels audio as WAV when the upstream sends no content type", async () => {
    mocks.speech.mockResolvedValue(new Response(new Uint8Array([1]), { status: 200, headers: { "content-type": "" } }));
    const res = await POST(request({ model: "tts", input: "hi" }));
    expect(res.headers.get("content-type")).toBe("audio/wav");
    expect(res.headers.get("x-obleth-request-id")).toBeNull();
  });
});
