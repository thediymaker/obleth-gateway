import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

type User = { id: string; role: string; status: string; tenantId: string | null };

/**
 * Minimal stand-in for a pg pool over an in-memory "user" table, interpreting
 * exactly the statements lib/auth/users.ts issues, so tests assert on the
 * resulting table state rather than on SQL text.
 */
function fakeDb(users: User[]) {
  const log: string[] = [];
  let released = 0;
  const client = {
    query: vi.fn(async (sql: string, params: unknown[] = []) => {
      const s = sql.trim().toLowerCase();
      log.push(s.split(/\s+/).slice(0, 2).join(" "));
      if (s.startsWith("select role, status")) {
        return { rows: users.filter((u) => u.id === params[0]).map(({ role, status }) => ({ role, status })) };
      }
      if (s.startsWith("select count(*)")) {
        const count = users.filter((u) => u.role === "admin" && u.status === "active" && u.id !== params[0]).length;
        return { rows: [{ count }] };
      }
      if (s.startsWith(`update "user" set role`)) {
        const u = users.find((x) => x.id === params[2]);
        if (u) Object.assign(u, { role: params[0], tenantId: params[1], status: "active" });
      } else if (s.startsWith(`update "user" set status`)) {
        const u = users.find((x) => x.id === params[1]);
        if (u) u.status = params[0] as string;
      }
      return { rows: [] };
    }),
    release: () => {
      released += 1;
    },
  };
  vi.doMock("@/lib/db", () => ({ getDb: () => ({ connect: async () => client }) }));
  return { users, log, released: () => released };
}

const TENANT = "11111111-1111-1111-1111-111111111111";

describe("assignUser", () => {
  it("writes role, tenant, and active status", async () => {
    const db = fakeDb([{ id: "u1", role: "user", status: "pending", tenantId: null }]);
    const { assignUser } = await import("./users");
    expect(await assignUser("u1", "user", TENANT)).toBe("ok");
    expect(db.users[0]).toEqual({ id: "u1", role: "user", status: "active", tenantId: TENANT });
    expect(db.log[0]).toBe("begin");
    expect(db.log[1]).toBe("select pg_advisory_xact_lock($1)");
    expect(db.log.at(-1)).toBe("commit");
    expect(db.released()).toBe(1);
  });

  it("refuses to demote the last active admin and leaves the row untouched", async () => {
    const db = fakeDb([
      { id: "a1", role: "admin", status: "active", tenantId: null },
      { id: "a2", role: "admin", status: "pending", tenantId: null },
    ]);
    const { assignUser } = await import("./users");
    expect(await assignUser("a1", "user", null)).toBe("last-admin");
    expect(db.users[0]).toMatchObject({ role: "admin", status: "active" });
    expect(db.log.at(-1)).toBe("rollback");
  });

  it("demotes an admin while another active admin remains", async () => {
    const db = fakeDb([
      { id: "a1", role: "admin", status: "active", tenantId: null },
      { id: "a2", role: "admin", status: "active", tenantId: null },
    ]);
    const { assignUser } = await import("./users");
    expect(await assignUser("a1", "user", null)).toBe("ok");
    expect(db.users[0].role).toBe("user");
  });

  it("reports an unknown user without writing", async () => {
    const db = fakeDb([]);
    const { assignUser } = await import("./users");
    expect(await assignUser("nope", "user", null)).toBe("not-found");
    expect(db.log).not.toContain(`update "user"`);
  });
});

describe("setUserStatus", () => {
  it("refuses to deactivate the last active admin", async () => {
    const db = fakeDb([{ id: "a1", role: "admin", status: "active", tenantId: null }]);
    const { setUserStatus } = await import("./users");
    expect(await setUserStatus("a1", "pending")).toBe("last-admin");
    expect(db.users[0].status).toBe("active");
  });

  it("deactivates a non-admin freely", async () => {
    const db = fakeDb([
      { id: "a1", role: "admin", status: "active", tenantId: null },
      { id: "u1", role: "user", status: "active", tenantId: null },
    ]);
    const { setUserStatus } = await import("./users");
    expect(await setUserStatus("u1", "pending")).toBe("ok");
    expect(db.users[1].status).toBe("pending");
  });

  it("rolls back and releases the client when a statement fails", async () => {
    const db = fakeDb([{ id: "u1", role: "user", status: "active", tenantId: null }]);
    const { getDb } = await import("@/lib/db");
    const client = await (getDb() as unknown as { connect: () => Promise<{ query: ReturnType<typeof vi.fn> }> }).connect();
    client.query.mockImplementationOnce(async () => ({ rows: [] })) // begin
      .mockImplementationOnce(async () => {
        throw new Error("lock failed");
      });
    const { setUserStatus } = await import("./users");
    await expect(setUserStatus("u1", "pending")).rejects.toThrow("lock failed");
    expect(db.users[0].status).toBe("active");
    expect(db.released()).toBe(1);
  });
});
