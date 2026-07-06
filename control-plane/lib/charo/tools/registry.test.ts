import { describe, it, expect, beforeEach } from "vitest";
import { registerTool, getTool, enabledTools, toolSchemas, __clearRegistry } from "./registry";
import type { CharoTool } from "./types";
import type { CharoSettingsView } from "@/lib/obleth";

const stub: CharoTool = {
  name: "stub", description: "d", parameters: { type: "object", properties: {} },
  resultType: "stub_result", requiresConfirmation: false,
  parseArgs: (r) => r, run: async () => ({ ok: true }),
};

const settings = (tools_enabled: Record<string, boolean>): CharoSettingsView => ({
  enabled: true, brain_model: "b", tools_enabled,
  bench_max_concurrency: 40, bench_max_duration_s: 120, bench_max_requests: 500,
});

describe("registry", () => {
  beforeEach(() => { __clearRegistry(); registerTool(stub); });

  it("looks a tool up by name", () => {
    expect(getTool("stub")).toBe(stub);
    expect(getTool("missing")).toBeUndefined();
  });

  it("missing key defaults to enabled", () => {
    expect(enabledTools(settings({})).map((t) => t.name)).toEqual(["stub"]);
  });

  it("explicit false disables", () => {
    expect(enabledTools(settings({ stub: false }))).toEqual([]);
  });

  it("builds OpenAI function schemas for enabled tools", () => {
    expect(toolSchemas(settings({}))).toEqual([
      { type: "function", function: { name: "stub", description: "d", parameters: { type: "object", properties: {} } } },
    ]);
  });
});
