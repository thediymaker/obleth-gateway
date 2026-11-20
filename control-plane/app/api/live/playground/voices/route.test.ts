import { beforeEach, describe, expect, it, vi } from "vitest";
import { NextRequest } from "next/server";
const mocks = vi.hoisted(() => ({ guard: vi.fn(), voices: vi.fn() }));
vi.mock("@/lib/auth/roles", () => ({ requireAdmin: mocks.guard }));
vi.mock("@/lib/charo/gateway", () => ({ gatewayVoices: mocks.voices }));
import { GET } from "./route";

const request = (model?: string) =>
  new NextRequest(`http://localhost/api/live/playground/voices${model === undefined ? "" : `?model=${encodeURIComponent(model)}`}`);
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

beforeEach(() => {
  vi.clearAllMocks();
  mocks.guard.mockResolvedValue({ email: "admin@example.edu" });
});

describe("GET /api/live/playground/voices", () => {
  it("denies unauthorized callers", async () => {
    mocks.guard.mockRejectedValue(new Error("not an admin"));
    expect((await GET(request("tts"))).status).toBe(401);
    expect(mocks.voices).not.toHaveBeenCalled();
  });

  it("needs a model", async () => {
    expect((await GET(request())).status).toBe(400);
    expect((await GET(request("x".repeat(201)))).status).toBe(400);
    expect(mocks.voices).not.toHaveBeenCalled();
  });

  it("returns the server's voices, uploaded ones included, without duplicates", async () => {
    mocks.voices.mockResolvedValue(json({ voices: ["tara", "leah", { name: "zac" }, "tara"], uploaded_voices: [{ voice_id: "mine" }] }));
    const res = await GET(request("my tts"));
    expect(mocks.voices.mock.calls[0][0]).toBe("my tts");
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ voices: ["tara", "leah", "zac", "mine"] });
  });

  it.each([404, 405, 501])("reports a %i as unsupported, not as an error", async (status) => {
    mocks.voices.mockResolvedValue(json({ error: { message: "not found" } }, status));
    const res = await GET(request("tts"));
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ voices: [], unsupported: true });
  });

  it("passes any other failure through with its message", async () => {
    mocks.voices.mockResolvedValue(json({ error: { message: "backend down" } }, 503));
    const res = await GET(request("tts"));
    expect(res.status).toBe(503);
    expect(await res.json()).toEqual({ error: "backend down" });
  });

  it("reports an unreachable gateway as a 502", async () => {
    mocks.voices.mockRejectedValue(new TypeError("fetch failed", { cause: new Error("connect ECONNREFUSED") }));
    const res = await GET(request("tts"));
    expect(res.status).toBe(502);
    expect(await res.json()).toEqual({ error: "The gateway could not be reached." });
  });
});
