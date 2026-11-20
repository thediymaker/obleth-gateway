// @vitest-environment node
import { createHash } from "node:crypto";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { NextRequest } from "next/server";

const mocks = vi.hoisted(() => ({ guard: vi.fn() }));
vi.mock("@/lib/auth/roles", () => ({ requireAdmin: mocks.guard }));
import { POST as upload } from "./knowledge/collections/[id]/documents/route";
import { GET as exportBackup } from "./backup/export/route";

const MiB = 1024 * 1024;
const params = { params: Promise.resolve({ id: "c1" }) };

/** A body of `size` bytes in 1 MiB chunks, with its digest. */
function synthetic(size: number) {
  const hash = createHash("sha256");
  const chunks: Uint8Array[] = [];
  for (let at = 0; at < size; at += MiB) {
    const chunk = new Uint8Array(Math.min(MiB, size - at)).map((_, i) => 65 + ((at + i) % 26));
    hash.update(chunk);
    chunks.push(chunk);
  }
  const stream = new ReadableStream<Uint8Array>({
    pull(c) {
      const next = chunks.shift();
      if (next) c.enqueue(next);
      else c.close();
    },
  });
  return { stream, digest: hash.digest("hex") };
}

async function digestOf(body: ReadableStream<Uint8Array>) {
  const hash = createHash("sha256");
  let size = 0;
  for await (const chunk of body as unknown as AsyncIterable<Uint8Array>) {
    hash.update(chunk);
    size += chunk.byteLength;
  }
  return { size, digest: hash.digest("hex") };
}

let upstream: { url: string; init: RequestInit & { duplex?: string } }[];
beforeEach(() => {
  vi.stubEnv("OBLETH_ADMIN_TOKEN", "test-token");
  mocks.guard.mockResolvedValue({ email: "admin@example.edu" });
  upstream = [];
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
});

describe("a knowledge upload", () => {
  it("streams the browser's body to the gateway byte for byte, without parsing it", async () => {
    let received: { size: number; digest: string } | null = null;
    vi.stubGlobal("fetch", vi.fn(async (url: string, init: RequestInit & { duplex?: string }) => {
      upstream.push({ url, init });
      received = await digestOf(init.body as ReadableStream<Uint8Array>);
      return new Response(JSON.stringify({ id: "d1", status: "pending" }), { status: 200, headers: { "content-type": "application/json" } });
    }));
    const body = synthetic(40 * MiB);
    const req = new NextRequest("http://localhost/api/live/knowledge/collections/c1/documents", {
      method: "POST",
      body: body.stream,
      headers: { "content-type": "application/json; charset=utf-8" },
      duplex: "half",
    } as ConstructorParameters<typeof NextRequest>[1]);
    const read = [vi.spyOn(req, "json"), vi.spyOn(req, "formData"), vi.spyOn(req, "text"), vi.spyOn(req, "arrayBuffer")];

    const res = await upload(req, params);

    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ id: "d1", status: "pending" });
    expect(received).toEqual({ size: 40 * MiB, digest: body.digest });
    for (const spy of read) expect(spy).not.toHaveBeenCalled();
    const [{ url, init }] = upstream;
    expect(url).toMatch(/\/api\/v1\/knowledge\/collections\/c1\/documents$/);
    expect(init.duplex).toBe("half");
    const headers = new Headers(init.headers);
    expect(headers.get("content-type")).toBe("application/json; charset=utf-8");
    expect(headers.get("authorization")).toBe("Bearer test-token");
  });

  it("refuses a declared length over the gateway's limit before sending anything", async () => {
    const fetch = vi.fn();
    vi.stubGlobal("fetch", fetch);
    const req = new NextRequest("http://localhost/api/live/knowledge/collections/c1/documents", {
      method: "POST",
      body: "{}",
      headers: { "content-type": "application/json", "content-length": String(80 * MiB) },
    });
    const res = await upload(req, params);
    expect(res.status).toBe(413);
    expect(fetch).not.toHaveBeenCalled();
  });

  it("passes the gateway's refusal on", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({ error: "file too large" }), { status: 400 })));
    const req = new NextRequest("http://localhost/api/live/knowledge/collections/c1/documents", { method: "POST", body: "{}", headers: { "content-type": "application/json" } });
    const res = await upload(req, params);
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "file too large" });
  });
});

describe("a backup export", () => {
  it("streams the gateway's body through and names the file", async () => {
    const body = synthetic(3 * MiB);
    vi.stubGlobal("fetch", vi.fn(async (url: string, init: RequestInit) => {
      upstream.push({ url, init });
      return new Response(body.stream, { status: 200, headers: { "content-type": "application/json" } });
    }));
    const res = await exportBackup(new NextRequest("http://localhost/api/live/backup/export"));
    expect(res.status).toBe(200);
    expect(res.headers.get("content-disposition")).toMatch(/^attachment; filename="obleth-backup-[\d-]+\.json"$/);
    expect(await digestOf(res.body!)).toEqual({ size: 3 * MiB, digest: body.digest });
    expect(new Headers(upstream[0].init.headers).get("x-obleth-audit-actor")).toBe("admin@example.edu");
  });

  it("passes the gateway's Content-Disposition through when it sends one", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response("{}", { status: 200, headers: { "content-type": "application/json", "content-disposition": 'attachment; filename="from-gateway.json"' } })));
    const res = await exportBackup(new NextRequest("http://localhost/api/live/backup/export"));
    expect(res.headers.get("content-disposition")).toBe('attachment; filename="from-gateway.json"');
  });
});
