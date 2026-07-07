import { describe, it, expect } from "vitest";
import { initialValues, boonOptions, collectArgs } from "./steps";
import type { Activity } from "./types";
import type { ModelRoute } from "@/lib/obleth";

const model = {
  id: "m1", model_name: "gemma4-31b-it", enabled: true,
  supports_vision: true, supports_function_calling: true, supports_response_schema: true,
  tool_servers: [], boons: [], // vision/tool via the supports_* flags
} as unknown as ModelRoute;

const bench: Activity = {
  id: "benchmark", label: "Benchmark", blurb: "", kind: "run",
  toolName: "run_benchmark", resultType: "bench_result",
  steps: [
    { type: "model", label: "Model" },
    { type: "number", key: "step_duration_s", label: "Seconds per step", default: 5, min: 1, max: 60 },
  ],
};

const test: Activity = {
  id: "test_capabilities", label: "Test", blurb: "", kind: "run",
  toolName: "test_capabilities", resultType: "capability_result",
  steps: [
    { type: "model", label: "Model" },
    { type: "checklist", key: "tests", label: "What to test", optionsFrom: "boons" },
  ],
};

describe("boonOptions", () => {
  it("always includes quick ping", () => {
    expect(boonOptions(undefined)).toEqual([{ value: "ping", label: "Quick ping", hint: "Confirms it responds" }]);
  });
  it("adds tools/json/vision from capabilities", () => {
    expect(boonOptions(model).map((o) => o.value)).toEqual(["ping", "tools", "json", "vision"]);
  });
});

describe("initialValues", () => {
  it("defaults numbers and selects all checklist options", () => {
    expect(initialValues(bench)).toEqual({ model: "", step_duration_s: 5 });
  });
});

describe("collectArgs", () => {
  it("maps model + number into tool args", () => {
    const args = collectArgs(bench, { model: "m1", step_duration_s: 10 }, model);
    expect(args).toEqual({ model: "gemma4-31b-it", step_duration_s: 10 });
  });
  it("maps a boon checklist into the tests array using resolved defaults", () => {
    const vals = initialValues(test, model); // seed defaults FROM the model → all four boons
    const args = collectArgs(test, { ...vals, model: "m1" }, model);
    expect(args).toEqual({ model: "gemma4-31b-it", tests: ["ping", "tools", "json", "vision"] });
  });
  it("respects unchecked options — returns the user's subset, not the model's full set", () => {
    // `model` supports vision, but the user unchecked it before submitting.
    const args = collectArgs(test, { model: "m1", tests: ["ping", "tools", "json"] }, model);
    expect(args).toEqual({ model: "gemma4-31b-it", tests: ["ping", "tools", "json"] });
  });
});
