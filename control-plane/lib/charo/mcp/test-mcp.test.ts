import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("@/lib/obleth", () => ({
  obleth: { listMcpServers: vi.fn(async () => []) },
}));
vi.mock("./probe", () => ({
  probeServer: vi.fn(async (name: string) => ({
    server: name,
    status: "pass",
    message: "handshake OK — 1 tool",
    serverInfo: null,
    protocolVersion: null,
    tools: [{ name: "echo", description: null }],
    latencyMs: null,
  })),
}));

import { obleth } from "@/lib/obleth";
import { probeServer } from "./probe";
import { testMcpTool } from "./test-mcp";
import type { ToolCtx } from "@/lib/charo/tools/types";

const server = (name: string, enabled = true) =>
  ({ id: name, name, upstream_url: "http://up", auth_header: null, enabled }) as never;

const ctx = (): ToolCtx =>
  ({ settings: {} as never, gatewayChat: (() => {}) as never, signal: new AbortController().signal });

describe("test_mcp tool", () => {
  beforeEach(() => {
    vi.mocked(obleth.listMcpServers).mockReset().mockResolvedValue([]);
    vi.mocked(probeServer).mockClear();
  });

  it("parseArgs: omitted/empty servers means all; rejects non-arrays; dedupes", () => {
    expect(testMcpTool.parseArgs({})).toEqual({ servers: null });
    expect(testMcpTool.parseArgs({ servers: [] })).toEqual({ servers: null });
    expect(testMcpTool.parseArgs({ servers: ["a", "a", " b "] })).toEqual({ servers: ["a", "b"] });
    expect(() => testMcpTool.parseArgs({ servers: "a" })).toThrow();
  });

  it("sweeps every registered server by default", async () => {
    vi.mocked(obleth.listMcpServers).mockResolvedValue([server("one"), server("two")]);
    const emitted: unknown[] = [];
    const result = await testMcpTool.run({ servers: null }, ctx(), (p) => emitted.push(p));
    expect(result.servers.map((r) => r.server)).toEqual(["one", "two"]);
    expect(probeServer).toHaveBeenCalledTimes(2);
    expect(emitted).toEqual([
      { kind: "mcp_probe", server: "one" },
      { kind: "mcp_probe", server: "two" },
    ]);
  });

  it("reports unknown names as failed rows without probing", async () => {
    vi.mocked(obleth.listMcpServers).mockResolvedValue([server("one")]);
    const result = await testMcpTool.run({ servers: ["ghost"] }, ctx(), () => {});
    expect(result.servers).toEqual([
      expect.objectContaining({ server: "ghost", status: "fail", message: "not registered" }),
    ]);
    expect(probeServer).not.toHaveBeenCalled();
  });

  it("skips disabled servers without probing", async () => {
    vi.mocked(obleth.listMcpServers).mockResolvedValue([server("off", false)]);
    const result = await testMcpTool.run({ servers: null }, ctx(), () => {});
    expect(result.servers[0]).toMatchObject({ server: "off", status: "skip", message: "disabled in registry" });
    expect(probeServer).not.toHaveBeenCalled();
  });
});
