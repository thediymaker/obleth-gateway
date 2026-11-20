import { afterEach, describe, expect, it, vi } from "vitest";
import { NextRequest } from "next/server";
afterEach(() => vi.resetModules());

function mockSession(user: unknown) {
  vi.doMock("@/lib/auth/session", () => ({ getSession: async () => user }));
}

describe("guardAdmin", () => {
  it("returns null (proceed) for an active admin", async () => {
    mockSession({ id: "u", email: "a", role: "admin", status: "active", tenantId: null });
    const { guardAdmin } = await import("./guard");
    expect(await guardAdmin()).toBeNull();
  });

  it("returns 401 for an active non-admin (portal user)", async () => {
    mockSession({ id: "u", email: "u", role: "user", status: "active", tenantId: "t" });
    const { guardAdmin } = await import("./guard");
    const res = await guardAdmin();
    expect(res).not.toBeNull();
    expect(res?.status).toBe(401);
  });

  it("returns 401 when there is no session at all", async () => {
    mockSession(null);
    const { guardAdmin } = await import("./guard");
    const res = await guardAdmin();
    expect(res?.status).toBe(401);
  });

  it("returns 401 for a pending admin (not yet active)", async () => {
    mockSession({ id: "u", email: "a", role: "admin", status: "pending", tenantId: null });
    const { guardAdmin } = await import("./guard");
    const res = await guardAdmin();
    expect(res?.status).toBe(401);
  });
});

describe("guardAdminSession", () => {
  it("returns the admin session alongside a null denial", async () => {
    const admin = { id: "u", email: "a@example.com", role: "admin", status: "active", tenantId: null };
    mockSession(admin);
    const { guardAdminSession } = await import("./guard");
    const { denied, session } = await guardAdminSession();
    expect(denied).toBeNull();
    expect(session).toEqual(admin);
  });

  it("returns a 401 and no session for a non-admin", async () => {
    mockSession({ id: "u", email: "u", role: "user", status: "active", tenantId: "t" });
    const { guardAdminSession } = await import("./guard");
    const { denied, session } = await guardAdminSession();
    expect(denied?.status).toBe(401);
    expect(session).toBeNull();
  });
});

describe("liveRoute", () => {
  const admin = { id: "u", email: "a@example.com", role: "admin", status: "active", tenantId: null };
  const req = () => new NextRequest("http://localhost/api/live/x");

  async function route(handler: Parameters<typeof import("./guard").liveRoute>[0], user: unknown = admin) {
    mockSession(user);
    const guard = await import("./guard");
    const { OblethApiError } = await import("@/lib/obleth");
    return { run: guard.liveRoute(handler), OblethApiError, RouteError: guard.RouteError };
  }

  it("refuses a non-admin before running the handler", async () => {
    let ran = false;
    const { run } = await route(async () => { ran = true; }, { ...admin, role: "user" });
    expect((await run(req())).status).toBe(401);
    expect(ran).toBe(false);
  });

  it("hands the handler its params and the admin's session, and sends its result as JSON", async () => {
    const { run } = await route(async (_req, { params, session }) => ({ id: (params as { id: string }).id, by: session.email }));
    const res = await run(req(), { params: Promise.resolve({ id: "m1" }) });
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ id: "m1", by: "a@example.com" });
  });

  it("passes a Response through untouched", async () => {
    const { run } = await route(async () => new Response("csv", { status: 201, headers: { "Content-Type": "text/csv" } }));
    const res = await run(req());
    expect(res.status).toBe(201);
    expect(await res.text()).toBe("csv");
  });

  it("keeps the gateway's status and message, without a class-name prefix", async () => {
    const { run, OblethApiError } = await route(async () => { throw new OblethApiError(409, "name taken", "/models"); });
    const res = await run(req());
    expect(res.status).toBe(409);
    expect(await res.json()).toEqual({ error: "name taken" });
  });

  it("sends a route's own refusal as it is", async () => {
    const { run, RouteError } = await route(async () => { throw new RouteError(400, "model is required"); });
    const res = await run(req());
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "model is required" });
  });

  it("calls an unreachable gateway a 502 and a slow one a 504", async () => {
    const down = await route(async () => { throw new TypeError("fetch failed"); });
    expect((await down.run(req())).status).toBe(502);
    const slow = await route(async () => { throw new DOMException("timed out", "TimeoutError"); });
    expect((await slow.run(req())).status).toBe(504);
  });

  it("hides a dashboard fault behind a plain 500", async () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    const { run } = await route(async () => { throw new TypeError("Cannot read properties of undefined (reading 'id')"); });
    const res = await run(req());
    expect(res.status).toBe(500);
    expect(await res.json()).toEqual({ error: "Unexpected error." });
    spy.mockRestore();
  });

  it("answers a body that isn't JSON with a 400", async () => {
    const guard = await import("./guard");
    const bad = new Request("http://localhost/x", { method: "POST", body: "{nope" });
    await expect(guard.jsonBody(bad)).rejects.toMatchObject({ status: 400 });
  });
});
