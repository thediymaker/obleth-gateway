import { describe, it, expect } from "vitest";
import { initialValues, boonOptions, collectArgs, visibleSteps } from "./steps";
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

const testWithImage: Activity = {
  ...test,
  steps: [
    ...test.steps,
    { type: "image", key: "image", label: "Test image", onlyWhen: { key: "tests", includes: "vision" } },
  ],
};

const DATA_URL = "data:image/png;base64,AAAA";

describe("visibleSteps", () => {
  it("hides a conditional image step while its trigger test is unchecked", () => {
    const steps = visibleSteps(testWithImage, { model: "m1", tests: ["ping", "json"] });
    expect(steps.map((s) => s.type)).toEqual(["model", "checklist"]);
  });
  it("shows the image step when the trigger test is checked", () => {
    const steps = visibleSteps(testWithImage, { model: "m1", tests: ["ping", "vision"] });
    expect(steps.map((s) => s.type)).toEqual(["model", "checklist", "image"]);
  });
  it("passes through activities with no conditional steps", () => {
    expect(visibleSteps(test, { model: "m1", tests: [] })).toEqual(test.steps);
  });
});

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
  it("defaults image steps to empty", () => {
    expect(initialValues(testWithImage, model).image).toBe("");
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
  it("includes the attached image when its step is visible", () => {
    const vals = { model: "m1", tests: ["ping", "vision"], image: DATA_URL };
    expect(collectArgs(testWithImage, vals, model).image).toBe(DATA_URL);
  });
  it("omits the image when vision is unchecked or nothing was attached", () => {
    expect(
      collectArgs(testWithImage, { model: "m1", tests: ["ping"], image: DATA_URL }, model).image,
    ).toBeUndefined();
    expect(
      collectArgs(testWithImage, { model: "m1", tests: ["ping", "vision"], image: "" }, model).image,
    ).toBeUndefined();
  });
});
