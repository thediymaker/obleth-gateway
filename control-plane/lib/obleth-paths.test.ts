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
});
