// @vitest-environment node
import { beforeEach, describe, expect, it, vi } from "vitest";
import { NextRequest } from "next/server";
const mocks = vi.hoisted(() => ({
  guard: vi.fn(),
  images: vi.fn(),
  search: vi.fn(),
  verdicts: vi.fn(),
  meshCreate: vi.fn(),
  meshGet: vi.fn(),
}));
vi.mock("@/lib/auth/roles", () => ({ requireAdmin: mocks.guard }));
vi.mock("@/lib/charo/gateway", () => ({
  gatewayImages: mocks.images,
  gatewaySearch: mocks.search,
  gatewayVerdicts: mocks.verdicts,
  gatewayMeshCreate: mocks.meshCreate,
  gatewayMeshGet: mocks.meshGet,
}));
import { POST as images } from "./images/route";
import { POST as search } from "./search/route";
import { POST as verdicts } from "./verdicts/route";
import { POST as meshCreate } from "./meshes/route";
import { GET as meshGet } from "./meshes/[id]/route";

const json = (path: string, body: unknown) =>
  new NextRequest(`http://localhost/api/live/playground/${path}`, { method: "POST", body: JSON.stringify(body) });

beforeEach(() => {
  vi.clearAllMocks();
  mocks.guard.mockResolvedValue({ email: "admin@example.edu" });
  for (const m of [mocks.images, mocks.search, mocks.verdicts, mocks.meshCreate, mocks.meshGet]) {
    m.mockImplementation(async () => new Response(JSON.stringify({ images: ["x"], data: [], results: [] }), { status: 200 }));
  }
});

// Closing the page aborts the browser's request; the relay hands that abort on,
// so the gateway stops the run (and its cost) too.
describe("playground relays pass the request's abort signal to the gateway", () => {
  it("images", async () => {
    const req = json("images", { model: "sdxl", prompt: "a cat" });
    await images(req);
    expect(mocks.images.mock.calls[0][1]).toBe(req.signal);
  });

  it("search", async () => {
    const req = json("search", { tool: "web", query: "obleth" });
    await search(req);
    expect(mocks.search.mock.calls[0][1]).toBe(req.signal);
  });

  it("verdicts", async () => {
    const req = json("verdicts", { model: "llama", state: "x", questions: { q: { type: "boolean", instructions: "Is it?" } } });
    await verdicts(req);
    expect(mocks.verdicts.mock.calls[0][1]).toBe(req.signal);
  });

  it("mesh jobs", async () => {
    const form = new FormData();
    form.set("model", "trellis");
    form.set("image", new File([new Uint8Array([1])], "a.png", { type: "image/png" }));
    const create = new NextRequest("http://localhost/api/live/playground/meshes", { method: "POST", body: form });
    await meshCreate(create);
    expect(mocks.meshCreate.mock.calls[0][1]).toBe(create.signal);

    const poll = new NextRequest("http://localhost/api/live/playground/meshes/j1");
    await meshGet(poll, { params: Promise.resolve({ id: "j1" }) });
    expect(mocks.meshGet.mock.calls[0][2]).toBe(poll.signal);
  });
});
