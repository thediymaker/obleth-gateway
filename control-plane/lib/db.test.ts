import { afterEach, describe, expect, it, vi } from "vitest";

describe("getDb", () => {
  afterEach(() => { vi.resetModules(); delete process.env.DATABASE_URL; });

  it("throws a clear error when DATABASE_URL is unset", async () => {
    delete process.env.DATABASE_URL;
    const { getDb } = await import("./db");
    expect(() => getDb()).toThrow(/DATABASE_URL/);
  });

  it("returns the same pool instance on repeated calls", async () => {
    process.env.DATABASE_URL = "postgres://obleth:obleth@localhost:5432/obleth";
    const { getDb } = await import("./db");
    expect(getDb()).toBe(getDb());
  });
});
