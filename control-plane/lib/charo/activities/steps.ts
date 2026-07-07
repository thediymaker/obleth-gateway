import type { ModelRoute } from "@/lib/obleth";
import type { Activity, StepSpec, StepValues } from "./types";

export interface Capability { value: string; label: string; hint?: string }

/** Capability test set derived from a model's boons (mirrors the old presetsFor). */
export function boonOptions(model: ModelRoute | undefined): Capability[] {
  const out: Capability[] = [{ value: "ping", label: "Quick ping", hint: "Confirms it responds" }];
  if (!model) return out;
  const hasTools = model.supports_function_calling || model.tool_servers.length > 0 || model.boons.includes("tool_loop");
  const hasJson = model.supports_response_schema || model.boons.includes("structured_output");
  const hasVision = model.supports_vision || model.boons.includes("vision");
  if (hasTools) out.push({ value: "tools", label: "Tools / web search", hint: "Exercises tool_loop" });
  if (hasJson) out.push({ value: "json", label: "Force JSON", hint: "structured_output" });
  if (hasVision) out.push({ value: "vision", label: "Describe image", hint: "vision — you'll attach one" });
  return out;
}

export function resolveChecklistOptions(
  step: Extract<StepSpec, { type: "checklist" }>,
  model: ModelRoute | undefined,
): Capability[] {
  return step.optionsFrom === "boons" ? boonOptions(model) : step.optionsFrom;
}

/**
 * Default step values. Model derivation happens later (the picker fills "model");
 * checklists select every option that applies to the currently-known model. When
 * no model is known yet the checklist defaults to every option that needs no model
 * ("ping"); the WorkflowCard re-derives on model change (see Task 4).
 */
export function initialValues(activity: Activity, model?: ModelRoute): StepValues {
  const vals: StepValues = {};
  for (const step of activity.steps) {
    if (step.type === "model") vals.model = "";
    else if (step.type === "number") vals[step.key] = step.default;
    else if (step.type === "checklist") {
      vals[step.key] = resolveChecklistOptions(step, model).map((o) => o.value);
    }
  }
  return vals;
}

/** Map collected step values into the backing tool's argument object. */
export function collectArgs(
  activity: Activity,
  values: StepValues,
  model: ModelRoute | undefined,
): Record<string, unknown> {
  const args: Record<string, unknown> = {};
  if (model) args.model = model.model_name;
  for (const step of activity.steps) {
    if (step.type === "number") args[step.key] = values[step.key];
    else if (step.type === "checklist") args[step.key] = values[step.key];
  }
  return args;
}
