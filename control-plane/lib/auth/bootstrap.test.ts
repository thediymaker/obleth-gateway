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

  it("swallows and logs a signup failure so startup still proceeds", async () => {
    const query = vi.fn().mockResolvedValueOnce({ rows: [{ count: "0" }] }); // admin count
    const signUp = vi.fn().mockRejectedValue(new Error("Password too short"));
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    vi.doMock("@/lib/db", () => ({ getDb: () => ({ query }) }));
    vi.doMock("@/lib/auth/better-auth", () => ({ auth: { api: { signUpEmail: signUp } } }));
    process.env.DASHBOARD_ADMIN_EMAIL = "admin@example.com";
    process.env.DASHBOARD_PASSWORD = "short";
    const { bootstrapAdmin } = await import("./bootstrap");
    // Must not reject — a bad password cannot crash startup.
    await expect(bootstrapAdmin()).resolves.toBeUndefined();
    expect(signUp).toHaveBeenCalledOnce();
    // The promote UPDATE must NOT run when signup failed.
    expect(query).toHaveBeenCalledOnce();
    expect(consoleError).toHaveBeenCalledWith(
      expect.stringContaining("failed to seed break-glass admin"),
    );
    expect(consoleError.mock.calls[0]?.[0]).toContain("Password too short");
    consoleError.mockRestore();
  });
});
