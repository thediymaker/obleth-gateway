// control-plane/components/charo/workflow-card.tsx
"use client";

import { useMemo, useState } from "react";
import { ChevronDown, Check } from "lucide-react";
import {
  DropdownMenu, DropdownMenuTrigger, DropdownMenuContent, DropdownMenuItem,
} from "@/components/ui/dropdown-menu";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";
import type { ModelRoute } from "@/lib/obleth";
import type { Activity, StepValues } from "@/lib/charo/activities/types";
import { initialValues, resolveChecklistOptions, collectArgs, boonOptions } from "@/lib/charo/activities/steps";
import { useEnabledModels } from "./use-enabled-models";

export function WorkflowCard({
  activity, onRun, onCancel,
}: {
  activity: Activity;
  onRun: (args: Record<string, unknown>) => void;
  onCancel: () => void;
}) {
  const { models } = useEnabledModels();
  const [values, setValues] = useState<StepValues>(() => initialValues(activity));
  const [stepIdx, setStepIdx] = useState(0);

  const model = useMemo(
    () => models.find((m) => m.id === values.model),
    [models, values.model],
  );

  // When the model changes, re-derive any boon checklist defaults to that model.
  const pickModel = (m: ModelRoute) => {
    setValues((prev) => {
      const next: StepValues = { ...prev, model: m.id };
      for (const step of activity.steps) {
        if (step.type === "checklist" && step.optionsFrom === "boons") {
          next[step.key] = boonOptions(m).map((o) => o.value);
        }
      }
      return next;
    });
  };

  const steps = activity.steps;
  const step = steps[stepIdx];
  const isLast = stepIdx === steps.length - 1;
  const canAdvance = step.type !== "model" || !!values.model;

  const run = () => onRun(collectArgs(activity, values, model));

  return (
    <div className="w-full overflow-hidden rounded-xl border border-violet-400/30 bg-secondary/20">
      <div className="flex items-center justify-between border-b border-border bg-secondary/30 px-3 py-2">
        <span className="text-xs font-semibold">
          {step.type === "model" ? activity.label : `Testing ${model?.model_name ?? ""}`}
        </span>
        <span className="text-[10px] uppercase tracking-wide text-muted-foreground">
          Step {stepIdx + 1} / {steps.length}
        </span>
      </div>

      <div className="space-y-2 px-3 py-3">
        {step.type === "model" && (
          <>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <button
                  type="button"
                  className="flex h-9 w-full items-center justify-between rounded-md border border-violet-400/40 bg-background px-3 text-sm shadow-sm hover:bg-accent/40"
                >
                  <span className="truncate">{model ? model.model_name : "Select a model…"}</span>
                  <ChevronDown className="ml-2 h-4 w-4 shrink-0 opacity-60" />
                </button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="start" className="z-[70] max-h-72 w-[var(--radix-dropdown-menu-trigger-width)] overflow-y-auto">
                {models.map((m) => (
                  <DropdownMenuItem key={m.id} onSelect={() => pickModel(m)} className="cursor-pointer justify-between gap-2">
                    <span className="truncate">{m.model_name}</span>
                    {m.id === values.model && <Check className="h-4 w-4 shrink-0 text-primary" />}
                  </DropdownMenuItem>
                ))}
              </DropdownMenuContent>
            </DropdownMenu>
            {model && (
              <div className="flex flex-wrap gap-1.5">
                {boonOptions(model).map((o) => <Badge key={o.value}>{o.label}</Badge>)}
              </div>
            )}
          </>
        )}

        {step.type === "number" && (
          <label className="flex items-center justify-between gap-3 text-sm">
            <span className="text-muted-foreground">{step.label}</span>
            <input
              type="number"
              min={step.min} max={step.max}
              value={Number(values[step.key] ?? step.default)}
              onChange={(e) => setValues((v) => ({ ...v, [step.key]: Number(e.target.value) }))}
              className="h-8 w-20 rounded-md border border-border bg-background px-2 text-right text-sm"
            />
          </label>
        )}

        {step.type === "checklist" && (
          <div className="space-y-1">
            {resolveChecklistOptions(step, model).map((o) => {
              const selected = (values[step.key] as string[]).includes(o.value);
              const toggle = () => setValues((v) => {
                const cur = new Set(v[step.key] as string[]);
                selected ? cur.delete(o.value) : cur.add(o.value);
                return { ...v, [step.key]: [...cur] };
              });
              return (
                <button key={o.value} type="button" onClick={toggle} className="flex w-full items-center gap-2 rounded-md px-1 py-1.5 text-left hover:bg-accent/40">
                  <span className={cn("flex h-4 w-4 items-center justify-center rounded border-2", selected ? "border-cyan-400 bg-cyan-400 text-background" : "border-muted-foreground/50")}>
                    {selected && <Check className="h-3 w-3" />}
                  </span>
                  <span className="text-sm">{o.label}</span>
                  {o.hint && <span className="ml-auto text-[10px] text-muted-foreground">{o.hint}</span>}
                </button>
              );
            })}
          </div>
        )}
      </div>

      <div className="flex items-center justify-between border-t border-border px-3 py-2">
        <button type="button" onClick={stepIdx === 0 ? onCancel : () => setStepIdx((i) => i - 1)} className="text-xs text-muted-foreground hover:text-foreground">
          {stepIdx === 0 ? "Cancel" : "← Back"}
        </button>
        {isLast ? (
          <button type="button" onClick={run} disabled={!canAdvance} className="rounded-md bg-primary px-3 py-1.5 text-xs font-semibold text-primary-foreground disabled:opacity-50">
            {activity.id === "benchmark" ? "Run benchmark" : "Run"}
          </button>
        ) : (
          <button type="button" onClick={() => setStepIdx((i) => i + 1)} disabled={!canAdvance} className="rounded-md bg-primary px-3 py-1.5 text-xs font-semibold text-primary-foreground disabled:opacity-50">
            Next →
          </button>
        )}
      </div>
    </div>
  );
}
