import { beforeEach, describe, expect, it, vi } from "vitest";

const { readSession } = vi.hoisted(() => ({ readSession: vi.fn() }));
vi.mock("better-auth/api", () => ({
  createAuthMiddleware: (handler: unknown) => handler,
  getAuthoritativeSessionFromCtx: readSession,
  APIError: class extends Error {
    constructor(public status: string, body: { message: string }) {
      super(body.message);
    }
  },
}));
import { requireActiveAccountForAdminApi } from "./admin-access";
const check = requireActiveAccountForAdminApi as unknown as (ctx: { path: string }) => Promise<void>;

beforeEach(() => readSession.mockReset());

describe("built-in admin API approval enforcement", () => {
  it.each([null, { user: { role: "admin", status: "pending" } }, { user: { role: "admin" } }])(
    "rejects absent or unapproved accounts: %j",
    async (session) => {
      readSession.mockResolvedValue(session);
      await expect(check({ path: "/admin/list-users" })).rejects.toMatchObject({ status: "FORBIDDEN" });
    },
  );
  it("blocks mutations after an existing administrator is marked pending", async () => {
    readSession.mockResolvedValueOnce({ user: { role: "admin", status: "active" } })
      .mockResolvedValueOnce({ user: { role: "admin", status: "pending" } });
    await expect(check({ path: "/admin/set-role" })).resolves.toBeUndefined();
    await expect(check({ path: "/admin/set-role" })).rejects.toMatchObject({ status: "FORBIDDEN" });
    expect(readSession).toHaveBeenCalledTimes(2);
  });
  it("leaves active-user permissions to the plugin, including ending impersonation", async () => {
    readSession.mockResolvedValue({ user: { role: "user", status: "active" } });
    await expect(check({ path: "/admin/stop-impersonating" })).resolves.toBeUndefined();
  });
  it.each(["/sign-in/email", "/sign-up/email", "/get-session", "/sign-out"])(
    "preserves public and pending-account auth flows: %s", async (path) => {
      await expect(check({ path })).resolves.toBeUndefined();
      expect(readSession).not.toHaveBeenCalled();
    },
  );
});
