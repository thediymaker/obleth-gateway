import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

vi.mock("@/lib/charo/gateway", () => ({
  getControlPlaneKey: vi.fn(async () => "sk-test"),
}));

import { probeServer } from "./probe";

function jsonResponse(body: unknown, headers: Record<string, string> = {}, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
}

function sseResponse(events: unknown[], headers: Record<string, string> = {}, status = 200): Response {
  const text = events.map((e) => `event: message\ndata: ${JSON.stringify(e)}\n\n`).join("");
  return new Response(text, {
    status,
    headers: { "content-type": "text/event-stream", ...headers },
  });
}

const INIT_RESULT = {
  jsonrpc: "2.0",
  id: 1,
  result: {
    protocolVersion: "2025-03-26",
    serverInfo: { name: "everything", version: "1.2.3" },
    capabilities: {},
  },
};
const TOOLS_RESULT = {
  jsonrpc: "2.0",
  id: 2,
  result: { tools: [{ name: "echo", description: "Echoes back" }, { name: "add" }] },
};

describe("probeServer", () => {
  const fetchMock = vi.fn();
  beforeEach(() => {
    fetchMock.mockReset();
    vi.stubGlobal("fetch", fetchMock);
  });
  afterEach(() => vi.unstubAllGlobals());

  it("passes on a JSON handshake and lists tools", async () => {
    fetchMock
      .mockResolvedValueOnce(jsonResponse(INIT_RESULT, { "mcp-session-id": "sess-1" }))
      .mockResolvedValueOnce(new Response(null, { status: 202 })) // notifications/initialized
      .mockResolvedValueOnce(jsonResponse(TOOLS_RESULT));

    const row = await probeServer("everything");
    expect(row.status).toBe("pass");
    expect(row.message).toBe("handshake OK — 2 tools");
    expect(row.serverInfo).toEqual({ name: "everything", version: "1.2.3" });
    expect(row.protocolVersion).toBe("2025-03-26");
    expect(row.tools).toEqual([
      { name: "echo", description: "Echoes back" },
      { name: "add", description: null },
    ]);
    expect(row.latencyMs?.initialize).toBeGreaterThanOrEqual(0);
    // Session id from initialize is echoed on later calls.
    const listHeaders = fetchMock.mock.calls[2][1].headers as Record<string, string>;
    expect(listHeaders["Mcp-Session-Id"]).toBe("sess-1");
    // Probe goes through the gateway with the control-plane key.
    expect(String(fetchMock.mock.calls[0][0])).toContain("/mcp/everything");
    const initHeaders = fetchMock.mock.calls[0][1].headers as Record<string, string>;
    expect(initHeaders.Authorization).toBe("Bearer sk-test");
  });

  it("parses SSE-encoded responses", async () => {
    fetchMock
      .mockResolvedValueOnce(sseResponse([INIT_RESULT]))
      .mockResolvedValueOnce(new Response(null, { status: 202 }))
      .mockResolvedValueOnce(sseResponse([TOOLS_RESULT]));

    const row = await probeServer("sse-server");
    expect(row.status).toBe("pass");
    expect(row.tools).toHaveLength(2);
  });

  it("fails with the gateway's message on a non-2xx (e.g. disabled server)", async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ error: "mcp server is disabled" }, {}, 403));
    const row = await probeServer("dead");
    expect(row.status).toBe("fail");
    expect(row.message).toBe("HTTP 403: mcp server is disabled");
  });

  it("fails on a JSON-RPC error from initialize", async () => {
    fetchMock.mockResolvedValueOnce(
      jsonResponse({ jsonrpc: "2.0", id: 1, error: { code: -32600, message: "bad init" } }),
    );
    const row = await probeServer("grumpy");
    expect(row.status).toBe("fail");
    expect(row.message).toBe("initialize failed: bad init");
  });

  it("warns when initialize succeeds but tools/list fails", async () => {
    fetchMock
      .mockResolvedValueOnce(jsonResponse(INIT_RESULT))
      .mockResolvedValueOnce(new Response(null, { status: 202 }))
      .mockResolvedValueOnce(jsonResponse({ jsonrpc: "2.0", id: 2, error: { message: "nope" } }));
    const row = await probeServer("half");
    expect(row.status).toBe("warn");
    expect(row.message).toBe("initialize OK, but tools/list failed: nope");
    expect(row.serverInfo?.name).toBe("everything");
  });

  it("detects a legacy SSE-only server as WARN", async () => {
    fetchMock
      .mockResolvedValueOnce(new Response("Method Not Allowed", { status: 405 })) // POST rejected
      .mockResolvedValueOnce(
        new Response("", { status: 200, headers: { "content-type": "text/event-stream" } }),
      ); // GET yields an event stream
    const row = await probeServer("legacy");
    expect(row.status).toBe("warn");
    expect(row.message).toContain("legacy SSE transport");
    expect(fetchMock.mock.calls[1][1].method).toBe("GET");
  });

  it("reports a timeout distinctly", async () => {
    const err = new Error("aborted");
    err.name = "TimeoutError";
    fetchMock.mockRejectedValueOnce(err);
    const row = await probeServer("slow");
    expect(row.status).toBe("fail");
    expect(row.message).toBe("initialize timed out after 10s");
  });
});
