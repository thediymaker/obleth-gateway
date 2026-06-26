import { afterEach, describe, expect, it, vi } from "vitest";

afterEach(() => {
  vi.resetModules();
  delete process.env.DASHBOARD_ADMIN_EMAIL;
  delete process.env.DASHBOARD_PASSWORD;
});

describe("bootstrapAdmin", () => {
  it("does nothing when an admin already exists", async () => {
    const query = vi.fn().mockResolvedValue({ rows: [{ count: "1" }] });
    const signUp = vi.fn();
    vi.doMock("@/lib/db", () => ({ getDb: () => ({ query }) }));
    vi.doMock("@/lib/auth/better-auth", () => ({ auth: { api: { signUpEmail: signUp } } }));
    process.env.DASHBOARD_ADMIN_EMAIL = "admin@example.com";
    process.env.DASHBOARD_PASSWORD = "supersecretpassword";
    const { bootstrapAdmin } = await import("./bootstrap");
    await bootstrapAdmin();
    expect(signUp).not.toHaveBeenCalled();
  });

  it("creates an admin when none exists and env is set", async () => {
    const query = vi
      .fn()
      .mockResolvedValueOnce({ rows: [{ count: "0" }] }) // admin count
      .mockResolvedValue({ rows: [] });                  // promote update
    const signUp = vi.fn().mockResolvedValue({ user: { id: "u1" } });
    vi.doMock("@/lib/db", () => ({ getDb: () => ({ query }) }));
    vi.doMock("@/lib/auth/better-auth", () => ({ auth: { api: { signUpEmail: signUp } } }));
    process.env.DASHBOARD_ADMIN_EMAIL = "admin@example.com";
    process.env.DASHBOARD_PASSWORD = "supersecretpassword";
    const { bootstrapAdmin } = await import("./bootstrap");
    await bootstrapAdmin();
    expect(signUp).toHaveBeenCalledOnce();
    expect(query).toHaveBeenLastCalledWith(
      expect.stringContaining("role = 'admin'"),
      ["u1"],
    );
  });
});
