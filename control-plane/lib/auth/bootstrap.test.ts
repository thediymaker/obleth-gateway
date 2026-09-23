// @vitest-environment node
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { memoryAdapter } from "better-auth/adapters/memory";

// These tests run the REAL better-auth configuration from better-auth.ts
// (disabled sign-up, the active-account admin hook, the admin plugin) against
// an in-memory store, so a seed path that the app's own hooks would refuse
// fails here instead of on a fresh production database.

type Row = Record<string, unknown>;
type Store = { user: Row[]; session: Row[]; account: Row[]; verification: Row[] };
type BetterAuthModule = typeof import("./better-auth");
type Auth = ReturnType<BetterAuthModule["createAuth"]>;

const EMAIL = "admin@example.com";
const PASSWORD = "supersecretpassword";

// One module-level mock whose `auth` is read per call: each test swaps in its
// own instance synchronously, so nothing left over from one test (even one that
// timed out) can re-point the mock at another test's store.
const current = vi.hoisted(() => ({ auth: null as unknown }));
vi.mock("@/lib/auth/better-auth", () => ({
  get auth() {
    if (!current.auth) throw new Error("test did not install an auth instance");
    return current.auth;
  },
}));

let createAuth: BetterAuthModule["createAuth"];
let bootstrapAdmin: typeof import("./bootstrap")["bootstrapAdmin"];

beforeAll(async () => {
  // The cold import of better-auth dominates the first run; pay it once here
  // under a generous timeout instead of inside whichever test runs first.
  ({ createAuth } = await vi.importActual<BetterAuthModule>("./better-auth"));
  ({ bootstrapAdmin } = await import("./bootstrap"));
}, 30_000);

function realAuth(store: Store): Auth {
  const instance = createAuth(memoryAdapter(store), "x".repeat(32));
  current.auth = instance;
  return instance;
}

function emptyStore(): Store {
  return { user: [], session: [], account: [], verification: [] };
}

afterEach(() => {
  current.auth = null;
  vi.restoreAllMocks();
  delete process.env.DASHBOARD_ADMIN_EMAIL;
  delete process.env.DASHBOARD_PASSWORD;
});

describe("bootstrapAdmin (real auth configuration)", { timeout: 30_000 }, () => {
  it("the public and admin create paths are both closed on a fresh database", async () => {
    const auth = realAuth(emptyStore());
    await expect(
      auth.api.signUpEmail({ body: { email: EMAIL, password: PASSWORD, name: EMAIL } }),
    ).rejects.toThrow("Email and password sign up is not enabled");
    await expect(
      auth.api.createUser({ body: { email: EMAIL, password: PASSWORD, name: EMAIL, role: "admin" } }),
    ).rejects.toThrow("An active account is required");
  });

  it("seeds an active, verified admin on a fresh database that can sign in", async () => {
    const store = emptyStore();
    const auth = realAuth(store);
    process.env.DASHBOARD_ADMIN_EMAIL = EMAIL;
    process.env.DASHBOARD_PASSWORD = PASSWORD;
    await bootstrapAdmin();

    expect(store.user).toHaveLength(1);
    expect(store.user[0]).toMatchObject({ email: EMAIL, role: "admin", status: "active", emailVerified: true });
    const signedIn = await auth.api.signInEmail({ body: { email: EMAIL, password: PASSWORD } });
    expect(signedIn.user.email).toBe(EMAIL);
  });

  it("is a no-op when an active admin already exists", async () => {
    const store = emptyStore();
    realAuth(store);
    process.env.DASHBOARD_ADMIN_EMAIL = EMAIL;
    process.env.DASHBOARD_PASSWORD = PASSWORD;
    await bootstrapAdmin();
    await bootstrapAdmin();
    expect(store.user).toHaveLength(1);
    expect(store.account).toHaveLength(1);
  });

  it("seeds when the only admin is inactive (only active admins count)", async () => {
    const store = emptyStore();
    const auth = realAuth(store);
    const ctx = await auth.$context;
    await ctx.internalAdapter.createUser({
      email: "former@example.com",
      name: "former",
      emailVerified: true,
      role: "admin",
      status: "pending",
    });
    process.env.DASHBOARD_ADMIN_EMAIL = EMAIL;
    process.env.DASHBOARD_PASSWORD = PASSWORD;
    await bootstrapAdmin();
    expect(store.user.find((u) => u.email === EMAIL)).toMatchObject({ role: "admin", status: "active" });
  });

  it("refuses a short password without creating a user or crashing startup", async () => {
    const store = emptyStore();
    realAuth(store);
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    process.env.DASHBOARD_ADMIN_EMAIL = EMAIL;
    process.env.DASHBOARD_PASSWORD = "short";
    await expect(bootstrapAdmin()).resolves.toBeUndefined();
    expect(store.user).toHaveLength(0);
    expect(consoleError).toHaveBeenCalledTimes(1);
    expect(consoleError.mock.calls[0]?.[0]).toContain("failed to seed break-glass admin");
    expect(consoleError.mock.calls[0]?.[0]).toContain("DASHBOARD_PASSWORD to >=8 chars");
  });

  it("does not re-promote an existing non-admin account with the break-glass email", async () => {
    const store = emptyStore();
    const auth = realAuth(store);
    const ctx = await auth.$context;
    await ctx.internalAdapter.createUser({ email: EMAIL, name: EMAIL, emailVerified: true, role: "user", status: "active" });
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    process.env.DASHBOARD_ADMIN_EMAIL = EMAIL;
    process.env.DASHBOARD_PASSWORD = PASSWORD;
    await expect(bootstrapAdmin()).resolves.toBeUndefined();
    expect(store.user).toHaveLength(1);
    expect(store.user[0]).toMatchObject({ role: "user" });
    expect(consoleError.mock.calls[0]?.[0]).toContain("already exists");
    expect(consoleError.mock.calls[0]?.[0]).not.toContain("DASHBOARD_PASSWORD");
  });

  it("propagates a database error instead of booting without knowing whether an admin exists", async () => {
    const store = emptyStore();
    const auth = realAuth(store);
    const ctx = await auth.$context;
    vi.spyOn(ctx.adapter, "count").mockRejectedValue(new Error("connect ECONNREFUSED 127.0.0.1:5432"));
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    process.env.DASHBOARD_ADMIN_EMAIL = EMAIL;
    process.env.DASHBOARD_PASSWORD = PASSWORD;
    await expect(bootstrapAdmin()).rejects.toThrow("ECONNREFUSED");
    expect(store.user).toHaveLength(0);
    expect(consoleError).not.toHaveBeenCalled();
  });

  it("does nothing when the break-glass env is unset", async () => {
    const store = emptyStore();
    realAuth(store);
    await bootstrapAdmin();
    expect(store.user).toHaveLength(0);
  });
});
