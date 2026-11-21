// @vitest-environment node
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { obleth, seg } from "./obleth";

describe("seg", () => {
  it("keeps an id inside its path segment", () => {
    expect(seg("3f2c1e6a-9d1b-4c7e-8a55-0d6f2b7c9e10")).toBe("3f2c1e6a-9d1b-4c7e-8a55-0d6f2b7c9e10");
    expect(seg(42)).toBe("42");
    expect(seg("a/b")).toBe("a%2Fb");
    expect(seg("../backup/restore?x=")).toBe("..%2Fbackup%2Frestore%3Fx%3D");
    expect(seg("name with space#frag")).toBe("name%20with%20space%23frag");
  });

  it("refuses what a URL parser would resolve away", () => {
    for (const id of ["", ".", ".."]) {
      expect(() => seg(id), JSON.stringify(id)).toThrow();
    }
  });
});

describe("Management API paths", () => {
  const calls: string[] = [];

  beforeEach(() => {
    calls.length = 0;
    vi.stubEnv("OBLETH_ADMIN_TOKEN", "test-token");
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string | URL) => {
        // What the admin server would be asked for, after URL normalization.
        calls.push(new URL(String(url)).pathname + new URL(String(url)).search);
        return new Response("{}", { status: 200, headers: { "content-type": "application/json" } });
      }),
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.unstubAllEnvs();
  });

  it("a crafted id cannot name another endpoint", async () => {
    await obleth.modelHealthDetail("../system/control-plane-key?");
    expect(calls[0]).toBe("/api/v1/models/..%2Fsystem%2Fcontrol-plane-key%3F/health");
    expect(() => obleth.modelHealthDetail("..")).toThrow();
    expect(calls).toHaveLength(1);
  });

  it("asks for only the key fields a picker needs", async () => {
    await obleth.listKeyFields(["id", "name", "key_prefix", "tenant_id"]);
    await obleth.listKeyFields(["tenant_id", "disabled"], "t1");
    expect(calls).toEqual(["/api/v1/keys?fields=id%2Cname%2Ckey_prefix%2Ctenant_id", "/api/v1/keys?fields=tenant_id%2Cdisabled&tenant_id=t1"]);
  });

  it("pages the keys with their filters, sort and cursor", async () => {
    const page = await obleth.listKeysPage(
      { q: "lab", tenantId: "t1", disabled: false, budget: "near", unused: true, sort: "name", dir: "desc", sinceMs: 1, untilMs: 2 },
      { limit: 100, cursor: "abc" },
    );
    expect(page.next).toBeNull();
    await obleth.countKeys({ kind: "identity" });
    await obleth.usageKeysSummary({ keyIds: ["a", "b"], sinceMs: 1, untilMs: 2, limit: 2 });
    await obleth.keyBudgets(["a"]);
    expect(calls).toEqual([
      "/api/v1/keys?tenant_id=t1&q=lab&disabled=false&budget=near&unused=true&sort=name&dir=desc&since_ms=1&until_ms=2&limit=100&cursor=abc",
      "/api/v1/keys/count?kind=identity",
      "/api/v1/usage/keys/summary?key_id=a%2Cb&since_ms=1&until_ms=2&limit=2",
      "/api/v1/budgets/usage?ids=a",
    ]);
  });
});
