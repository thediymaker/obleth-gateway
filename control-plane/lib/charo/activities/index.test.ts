import { describe, it, expect, beforeEach } from "vitest";
import { __clearActivities, listActivities, getActivity } from "./registry";
import { ensureActivitiesRegistered, __resetActivitiesBootstrap } from "./index";
import { collectArgs, initialValues } from "./steps";

describe("ensureActivitiesRegistered", () => {
  beforeEach(() => {
    __clearActivities();
    __resetActivitiesBootstrap();
  });

  it("registers the benchmark activity backed by run_benchmark", () => {
    ensureActivitiesRegistered();
    const a = getActivity("benchmark");
    expect(a?.toolName).toBe("run_benchmark");
    expect(a?.resultType).toBe("bench_result");
    expect(a?.kind).toBe("run");
    expect(a?.steps[0]).toEqual({ type: "model", label: "Model" });
  });

  it("is idempotent (no duplicate registration)", () => {
    ensureActivitiesRegistered();
    ensureActivitiesRegistered();
    expect(listActivities().filter((x) => x.id === "benchmark")).toHaveLength(1);
  });

  it("benchmark collectArgs produces a run_benchmark payload", () => {
    ensureActivitiesRegistered();
    const a = getActivity("benchmark")!;
    const model = { id: "m1", model_name: "llama3-70b" } as never;
    const args = collectArgs(a, { ...initialValues(a), model: "m1" }, model);
    expect(args).toMatchObject({ model: "llama3-70b", step_duration_s: 5 });
  });

  it("registers test_capabilities as the first activity, backed by its tool", () => {
    ensureActivitiesRegistered();
    const a = getActivity("test_capabilities");
    expect(a?.toolName).toBe("test_capabilities");
    expect(a?.resultType).toBe("capability_result");
    expect(a?.steps.map((s) => s.type)).toEqual(["model", "checklist"]);
    expect(listActivities()[0]?.id).toBe("test_capabilities"); // leads the launcher
  });

  it("registers chat_with_model as a target activity with a single model step", () => {
    ensureActivitiesRegistered();
    const a = getActivity("chat_with_model");
    expect(a?.kind).toBe("target");
    expect(a?.toolName).toBeUndefined();
    expect(a?.steps.map((s) => s.type)).toEqual(["model"]);
    // Order: test_capabilities, chat_with_model, benchmark
    expect(listActivities().map((x) => x.id)).toEqual(["test_capabilities", "chat_with_model", "benchmark"]);
  });
});
