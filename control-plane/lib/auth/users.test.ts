import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

it("assignUser writes role, tenant, and active status", async () => {
  const query = vi.fn().mockResolvedValue({ rows: [] });
  vi.doMock("@/lib/db", () => ({ getDb: () => ({ query }) }));
  const { assignUser } = await import("./users");
  await assignUser("u1", "user", "11111111-1111-1111-1111-111111111111");
  expect(query).toHaveBeenCalledWith(
    expect.stringContaining(`update "user"`),
    ["user", "11111111-1111-1111-1111-111111111111", "u1"],
  );
});
