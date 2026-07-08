import { describe, it, expect } from "vitest";
import { evaluateTest, TEST_CATALOG } from "./catalog";
import type { TraceSummary } from "@/lib/charo/trace";

const trace = (over: Partial<TraceSummary> = {}): TraceSummary => ({
  model: "m", boonsFired: [], toolLoopIters: 0, toolsCalled: [], cacheStatus: "off",
  inputTokens: 0, outputTokens: 0, ttftMs: 0, totalMs: 0, costUsd: 0, statusCode: 200,
  errorStages: [], ...over,
});

describe("TEST_CATALOG", () => {
  it("has a prompt for every test and marks vision as needing an image", () => {
    expect(TEST_CATALOG.ping.prompt.length).toBeGreaterThan(0);
    expect(TEST_CATALOG.vision.needsImage).toBe(true);
    expect(TEST_CATALOG.tools.needsImage).toBeFalsy();
  });
});

describe("evaluateTest", () => {
  it("ping passes on a non-empty 200 reply", () => {
    expect(evaluateTest("ping", { ok: true, content: "Hi." }, trace()).status).toBe("pass");
  });
  it("ping fails on an empty reply", () => {
    expect(evaluateTest("ping", { ok: true, content: "" }, trace()).status).toBe("fail");
  });
  it("json passes on parseable JSON, fails on prose", () => {
    expect(evaluateTest("json", { ok: true, content: '{"status":"ok"}' }, trace()).status).toBe("pass");
    expect(evaluateTest("json", { ok: true, content: "sure, here you go" }, trace()).status).toBe("fail");
  });
  it("tools passes only when the trace shows tool_loop fired", () => {
    expect(evaluateTest("tools", { ok: true, content: "octopuses have 3 hearts" }, trace({ toolLoopIters: 1, boonsFired: ["tool_loop"], toolsCalled: ["web_search"] })).status).toBe("pass");
  });
  it("tools warns when it answered but no tool call was recorded", () => {
    expect(evaluateTest("tools", { ok: true, content: "octopuses have 3 hearts" }, trace()).status).toBe("warn");
  });
  it("tools warns when the trace is unavailable", () => {
    expect(evaluateTest("tools", { ok: true, content: "answer" }, null).status).toBe("warn");
  });
  it("vision passes when the vision boon fired without error", () => {
    expect(evaluateTest("vision", { ok: true, content: "a red square" }, trace({ boonsFired: ["vision"] })).status).toBe("pass");
  });
  it("any test fails on a non-ok response", () => {
    expect(evaluateTest("ping", { ok: false, content: "" }, trace({ statusCode: 500 })).status).toBe("fail");
    expect(evaluateTest("vision", { ok: false, content: "" }, null).status).toBe("fail");
  });
  it("vision fails when the vision boon reported an error stage", () => {
    expect(evaluateTest("vision", { ok: true, content: "" }, trace({ errorStages: ["boon:vision"] })).status).toBe("fail");
  });
});
