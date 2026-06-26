import { describe, expect, it, vi } from "vitest";

describe("applyAuthSchema", () => {
  it("runs the schema SQL against the pool exactly once", async () => {
    const query = vi.fn().mockResolvedValue({ rows: [] });
    vi.doMock("@/lib/db", () => ({ getDb: () => ({ query }) }));
    const { applyAuthSchema } = await import("./schema");
    await applyAuthSchema();
    await applyAuthSchema();
    expect(query).toHaveBeenCalledTimes(1);
    vi.resetModules();
  });
});
