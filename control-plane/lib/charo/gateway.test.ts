import { describe, it, expect, vi, beforeEach } from "vitest";
import { gatewayChat, gatewayImages, getControlPlaneKey, __resetKeyCache } from "./gateway";

describe("gatewayChat", () => {
  beforeEach(() => {
    __resetKeyCache();
    process.env.OBLETH_ADMIN_TOKEN = "admin-test";
    process.env.OBLETH_ADMIN_BASE_URL = "http://localhost:9180";
    process.env.OBLETH_PROXY_BASE_URL = "http://localhost:9080";
  });

  it("sends the system key as bearer and posts to the proxy base", async () => {
    const fetchMock = vi.fn(async (url: string | URL, init?: RequestInit) => {
      const u = String(url);
      if (u.includes("/system/control-plane-key")) {
        return new Response(JSON.stringify({ secret: "sk-test" }), { status: 200 });
      }
      expect(u).toBe("http://localhost:8080/v1/chat/completions");
      const headers = init?.headers as Record<string, string>;
      expect(headers.Authorization).toBe("Bearer sk-test");
      expect(init?.method).toBe("POST");
      return new Response("{}", {
        status: 200,
        headers: { "x-obleth-request-id": "rid-1" },
      });
    });
    vi.stubGlobal("fetch", fetchMock);

    const res = await gatewayChat({ model: "m", messages: [] });
    expect(res.headers.get("x-obleth-request-id")).toBe("rid-1");
  });

  it("posts image generations to /v1/images/generations", async () => {
    const fetchMock = vi.fn(async (url: string | URL, init?: RequestInit) => {
      const u = String(url);
      if (u.includes("/system/control-plane-key")) {
        return new Response(JSON.stringify({ secret: "sk-test" }), { status: 200 });
      }
      expect(u).toBe("http://localhost:8080/v1/images/generations");
      const headers = init?.headers as Record<string, string>;
      expect(headers.Authorization).toBe("Bearer sk-test");
      expect(JSON.parse(String(init?.body))).toMatchObject({ model: "img", prompt: "a cat" });
      return new Response("{}", { status: 200 });
    });
    vi.stubGlobal("fetch", fetchMock);

    const res = await gatewayImages({ model: "img", prompt: "a cat" });
    expect(res.status).toBe(200);
  });

  it("caches the key secret across calls", async () => {
    let keyFetches = 0;
    const fetchMock = vi.fn(async (url: string | URL) => {
      const u = String(url);
      if (u.includes("/system/control-plane-key")) {
        keyFetches++;
        return new Response(JSON.stringify({ secret: "sk-test" }), { status: 200 });
      }
      return new Response("{}", { status: 200 });
    });
    vi.stubGlobal("fetch", fetchMock);

    await getControlPlaneKey();
    await getControlPlaneKey();
    expect(keyFetches).toBe(1);
  });
});
