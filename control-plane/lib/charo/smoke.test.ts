import { describe, it, expect } from "vitest";

// Temporary smoke test: proves vitest runs, jsdom is present, and the `@/`
// alias resolves. Deleted once the real Charo tests land.
describe("vitest smoke", () => {
  it("has a jsdom window", () => {
    expect(typeof window).toBe("object");
    expect(typeof localStorage).toBe("object");
  });
  it("resolves the @/ alias", async () => {
    const mod = await import("@/lib/utils");
    expect(mod).toBeTruthy();
  });
});
