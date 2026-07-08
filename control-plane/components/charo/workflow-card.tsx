// control-plane/components/charo/workflow-card.tsx
"use client";

import { useMemo, useRef, useState } from "react";
import { ChevronDown, Check, ImagePlus, X } from "lucide-react";
import {
  DropdownMenu, DropdownMenuTrigger, DropdownMenuContent, DropdownMenuItem,
} from "@/components/ui/dropdown-menu";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";
import { Rail, MicroLabel } from "@/components/charo/rail";
import type { ModelRoute } from "@/lib/obleth";
import type { Activity, StepValues } from "@/lib/charo/activities/types";
import { initialValues, resolveChecklistOptions, collectArgs, boonOptions, visibleSteps } from "@/lib/charo/activities/steps";
import { useEnabledModels } from "./use-enabled-models";

// Pick-or-drop field for an image step (e.g. the vision test's subject image).
// `data-charo-dropzone` tells the panel's global drop handler to leave drops
// here alone instead of also attaching them to the composer.
function ImageField({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [error, setError] = useState<string | null>(null);
  const readFile = (files: FileList | null) => {
    const file = files?.[0];
    if (!file) return;
    if (!file.type.startsWith("image/")) { setError("Only images can be attached."); return; }
    if (file.size > 6 * 1024 * 1024) { setError("Image is too large (max 6 MB)."); return; }
    const reader = new FileReader();
    reader.onload = () => { onChange(String(reader.result)); setError(null); };
    reader.readAsDataURL(file);
  };
  return (
    <div className="space-y-1" data-charo-dropzone="">
      <input
        ref={inputRef}
        type="file"
        accept="image/*"
        className="hidden"
        onChange={(e) => { readFile(e.target.files); e.currentTarget.value = ""; }}
      />
      {value ? (
        <div className="relative inline-block">
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img src={value} alt="vision test subject" className="max-h-28 rounded-md" />
          <button
            type="button"
            title="Remove image"
            onClick={() => onChange("")}
            className="absolute -right-2 -top-2 flex h-5 w-5 items-center justify-center rounded-full border border-border bg-background text-muted-foreground hover:text-foreground"
          >
            <X className="h-3 w-3" />
          </button>
        </div>
      ) : (
        <button
          type="button"
          onClick={() => inputRef.current?.click()}
          onDragOver={(e) => e.preventDefault()}
          onDrop={(e) => { e.preventDefault(); readFile(e.dataTransfer.files); }}
          className="flex h-24 w-full flex-col items-center justify-center gap-1 rounded-md border border-dashed border-violet-400/40 text-muted-foreground hover:bg-violet-500/[0.05]"
        >
          <ImagePlus className="h-4 w-4" />
          <span className="text-[11.5px]">Click or drop an image — the vision test asks the model to describe it</span>
        </button>
      )}
      {error && <p className="text-[11.5px] text-destructive">{error}</p>}
    </div>
  );
}

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

  // Conditional steps (the vision test's image) appear and disappear with
  // checklist choices, so derive the visible list per render and clamp the index.
  const steps = visibleSteps(activity, values);
  const idx = Math.min(stepIdx, steps.length - 1);
  const step = steps[idx];
  const isLast = idx === steps.length - 1;
  const canAdvance =
    step.type === "model" ? !!values.model
    : step.type === "image" ? typeof values[step.key] === "string" && !!values[step.key]
    : true;

  const run = () => onRun(collectArgs(activity, values, model));

  return (
    <Rail className="w-full">
      <div className="mb-2 flex items-baseline justify-between">
        <span className="text-[13px] font-semibold text-foreground">
          {step.type === "model" ? activity.label : `Testing ${model?.model_name ?? ""}`}
        </span>
        <MicroLabel>Step {idx + 1} / {steps.length}</MicroLabel>
      </div>

      <div className="space-y-2">
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

        {step.type === "image" && (
          <ImageField
            value={typeof values[step.key] === "string" ? (values[step.key] as string) : ""}
            onChange={(v) => setValues((prev) => ({ ...prev, [step.key]: v }))}
          />
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

      <div className="mt-3 flex items-center justify-between">
        <button type="button" onClick={idx === 0 ? onCancel : () => setStepIdx(idx - 1)} className="text-xs text-muted-foreground hover:text-foreground">
          {idx === 0 ? "Cancel" : "← Back"}
        </button>
        {isLast ? (
          <button type="button" onClick={run} disabled={!canAdvance} className="rounded-md bg-primary px-3 py-1.5 text-xs font-semibold text-primary-foreground disabled:opacity-50">
            {activity.id === "benchmark" ? "Run benchmark" : "Run"}
          </button>
        ) : (
          <button type="button" onClick={() => setStepIdx(idx + 1)} disabled={!canAdvance} className="rounded-md bg-primary px-3 py-1.5 text-xs font-semibold text-primary-foreground disabled:opacity-50">
            Next →
          </button>
        )}
      </div>
    </Rail>
  );
}
