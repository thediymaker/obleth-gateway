import { describe, it, expect, beforeEach } from "vitest";
import { registerTool, __clearRegistry } from "./registry";
import { runTool } from "./executor";
import type { CharoTool, ToolCtx } from "./types";

const ctx = (): ToolCtx => ({
  settings: { enabled: true, brain_model: "b", tools_enabled: {}, bench_max_concurrency: 40, bench_max_duration_s: 120, bench_max_requests: 500 },
  gatewayChat: (async () => new Response()) as ToolCtx["gatewayChat"],
  signal: new AbortController().signal,
});

const ok: CharoTool = {
  name: "ok", description: "", parameters: {}, resultType: "ok_result",
  requiresConfirmation: false, parseArgs: (r) => r,
  run: async () => ({ value: 7 }),
};
const boom: CharoTool = {
  name: "boom", description: "", parameters: {}, resultType: "x",
  requiresConfirmation: false, parseArgs: (r) => r,
  run: async () => { throw new Error("kaboom"); },
};

describe("runTool", () => {
  beforeEach(() => { __clearRegistry(); registerTool(ok); registerTool(boom); });

  it("wraps a result in the tool's resultType envelope", async () => {
    const env = await runTool("ok", {}, ctx(), () => {});
    expect(env).toEqual({ type: "ok_result", data: { value: 7 } });
  });

  it("unknown tool → tool_error", async () => {
    const env = await runTool("nope", {}, ctx(), () => {});
    expect(env.type).toBe("tool_error");
  });

  it("a throwing tool → tool_error envelope, does not reject", async () => {
    const env = await runTool("boom", {}, ctx(), () => {});
    expect(env.type).toBe("tool_error");
    expect((env.data as { message: string }).message).toContain("kaboom");
  });
});
