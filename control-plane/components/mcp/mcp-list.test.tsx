// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { setModelToolServerAction } from "@/app/actions";
import { McpList } from "./mcp-list";
import type { McpServer, ModelRoute } from "@/lib/obleth";

vi.mock("@/app/actions", () => ({
  createMcpServerAction: vi.fn(async () => ({ ok: true })),
  deleteMcpServerAction: vi.fn(async () => ({ ok: true })),
  saveMcpServerAction: vi.fn(async () => ({ ok: true })),
  setMcpServerEnabledAction: vi.fn(async () => ({ ok: true })),
  setModelToolServerAction: vi.fn(async () => ({ ok: true })),
}));
vi.mock("next/navigation", () => ({ useRouter: () => ({ refresh: vi.fn(), push: vi.fn() }) }));

const servers: McpServer[] = [
  { id: "s1", name: "github", upstream_url: "https://gh.example/mcp", auth_header_set: true, enabled: true, created_at: "", updated_at: "" },
  { id: "s2", name: "jira", upstream_url: "https://jira.example/mcp", auth_header_set: false, enabled: true, created_at: "", updated_at: "" },
];
const models = [
  { id: "m1", model_name: "kimi", model_type: "chat", supports_function_calling: true, tool_servers: ["github"] },
  { id: "m2", model_name: "glm", model_type: "chat", supports_function_calling: true, tool_servers: [] },
] as unknown as ModelRoute[];

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  globalThis.fetch = vi.fn(async (url: string) => {
    const ok = String(url).includes("github");
    return new Response(JSON.stringify({ server: ok ? "github" : "jira", status: ok ? "pass" : "fail", message: ok ? "handshake OK" : "connection refused", serverInfo: ok ? { name: "github-mcp", version: "1.4.0" } : null, protocolVersion: null, tools: ok ? [{ name: "search_repositories", description: "Search repos" }] : null, latencyMs: ok ? { initialize: 200, toolsList: 12 } : null }), { status: 200 });
  }) as unknown as typeof fetch;
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

describe("MCP servers", () => {
  it("checks each server, flags the one that doesn't answer, and grants a model its tools", async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, enabled: false } } });
    await act(async () => root.render(<QueryClientProvider client={client}><McpList servers={servers} models={models} initialStats={null} /></QueryClientProvider>));
    await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
    const table = host.querySelector('[aria-label="Servers"]')!;
    expect(table.textContent).toContain("github-mcp 1.4.0");
    expect(table.textContent).toContain("212 ms");
    expect(table.textContent).toContain("Not answering");
    expect(host.textContent).toContain("jira: connection refused");
    await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="Open github"]')!.click());
    expect(document.body.textContent).toContain("search_repositories");
    const glm = document.querySelector<HTMLInputElement>('input[aria-label="glm can use github"]')!;
    await act(async () => glm.click());
    expect(setModelToolServerAction).toHaveBeenCalledWith("m2", "github", true);
  });
});
