"use client";
import React, { useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  recommendNCpuMoe,
  type RecipeParam,
  type SlurmRecipe,
} from "@/lib/model-recipes";

// Humanise a token count for the slider readout (4096 to "4K", 1048576 to "1M").
function humanizeCount(n: number): string {
  if (n >= 1_048_576 && n % 1_048_576 === 0) return `${n / 1_048_576}M`;
  if (n >= 1024 && n % 1024 === 0) return `${n / 1024}K`;
  return String(n);
}

// Slider for a numeric knob. Two modes: discrete `steps` (snap to a list, e.g.
// context sizes) or a continuous `min`/`max`/`step` range. Shows a live readout
// of the current value next to the label.
function ParamSlider({
  param,
  value,
  onChange,
}: {
  param: RecipeParam;
  value: string;
  onChange: (value: string) => void;
}): React.ReactElement {
  const useSteps = (param.steps?.length ?? 0) > 0;
  const current = Number(value) || 0;

  let sliderMin: number;
  let sliderMax: number;
  let sliderStep: number;
  let sliderValue: number;
  let readout: string;

  if (useSteps) {
    const steps = param.steps!;
    const idx = (() => {
      const exact = steps.indexOf(current);
      if (exact >= 0) return exact;
      // nearest stop
      let best = 0;
      for (let i = 1; i < steps.length; i++) {
        if (Math.abs(steps[i] - current) < Math.abs(steps[best] - current)) best = i;
      }
      return best;
    })();
    sliderMin = 0;
    sliderMax = steps.length - 1;
    sliderStep = 1;
    sliderValue = idx;
    readout = humanizeCount(steps[idx]);
  } else {
    sliderMin = param.min ?? 0;
    sliderMax = param.max ?? 100;
    sliderStep = param.step ?? 1;
    sliderValue = current;
    readout = String(current);
  }

  return (
    <div className="space-y-1.5">
      <div className="flex items-baseline justify-between">
        <Label htmlFor={`recipe-${param.id}`}>{param.label}</Label>
        <span className="font-mono text-xs tabular-nums text-foreground">
          {readout}
        </span>
      </div>
      <input
        id={`recipe-${param.id}`}
        type="range"
        min={sliderMin}
        max={sliderMax}
        step={sliderStep}
        value={sliderValue}
        onChange={(e) => {
          const raw = Number(e.target.value);
          onChange(useSteps ? String(param.steps![raw]) : String(raw));
        }}
        className="h-2 w-full cursor-pointer appearance-none rounded-full bg-muted accent-primary"
      />
      {param.hint && <p className="text-xs text-muted-foreground">{param.hint}</p>}
    </div>
  );
}

// Self-contained renderer for one recipe knob, mirroring the original
// `RecipeParamField` in model-manager.tsx (boolean=checkbox, select=native
// <select> of param.options, number with a range=slider, number/text=Input).
function ParamField({
  param,
  value,
  onChange,
}: {
  param: RecipeParam;
  value: string;
  onChange: (value: string) => void;
}): React.ReactElement {
  if (param.kind === "boolean") {
    return (
      <label className="flex items-start gap-2 text-sm sm:col-span-2">
        <input
          type="checkbox"
          className="mt-0.5 h-4 w-4 rounded border-border accent-primary"
          checked={value === "true"}
          onChange={(e) => onChange(e.target.checked ? "true" : "false")}
        />
        <span>
          <span className="block font-medium">{param.label}</span>
          {param.hint && (
            <span className="mt-0.5 block text-xs text-muted-foreground">
              {param.hint}
            </span>
          )}
        </span>
      </label>
    );
  }
  const hasRange =
    (param.steps?.length ?? 0) > 0 ||
    (param.min != null && param.max != null);
  if (param.kind === "number" && hasRange) {
    return <ParamSlider param={param} value={value} onChange={onChange} />;
  }
  if (param.kind === "select") {
    return (
      <div className="space-y-1.5">
        <Label htmlFor={`recipe-${param.id}`}>{param.label}</Label>
        <select
          id={`recipe-${param.id}`}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className="flex h-9 w-full rounded-md border border-input bg-background px-3 py-1 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
        >
          {param.options?.map((opt) => (
            <option key={opt.value} value={opt.value}>
              {opt.label}
            </option>
          ))}
        </select>
        {param.hint && (
          <p className="text-xs text-muted-foreground">{param.hint}</p>
        )}
      </div>
    );
  }
  return (
    <div className="space-y-1.5">
      <Label htmlFor={`recipe-${param.id}`}>{param.label}</Label>
      <Input
        id={`recipe-${param.id}`}
        type={param.kind === "number" ? "number" : "text"}
        value={value}
        placeholder={param.placeholder}
        onChange={(e) => onChange(e.target.value)}
        autoCapitalize="none"
        autoCorrect="off"
        spellCheck={false}
        className="h-9 text-xs"
      />
      {param.hint && <p className="text-xs text-muted-foreground">{param.hint}</p>}
    </div>
  );
}

// The empirical n-cpu-moe tuning hint. Always advisory: shows a recommendation
// only when GPU VRAM is known, with a "Use" action - never auto-applies.
function NCpuMoeHint({
  values,
  vramGb,
  onChange,
}: {
  values: Record<string, string>;
  vramGb: number | null;
  onChange: (id: string, value: string) => void;
}): React.ReactElement {
  if (vramGb == null) {
    return (
      <p className="text-xs text-muted-foreground/70">
        Enter GPU VRAM (GB) above to get a suggested CPU MoE layer count.
      </p>
    );
  }
  const rec = recommendNCpuMoe(Number(values.ctx_size || 0), vramGb);
  if (rec == null) {
    return (
      <p className="text-xs text-muted-foreground/70">
        Enter GPU VRAM (GB) above to get a suggested CPU MoE layer count.
      </p>
    );
  }
  return (
    <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
      <span>
        Recommended approx. {rec} for ~{vramGb}GB @ {values.ctx_size || 0} ctx
      </span>
      <Button
        type="button"
        variant="outline"
        size="sm"
        className="h-6 px-2 text-xs"
        onClick={() => onChange("n_cpu_moe", String(rec))}
      >
        Use
      </Button>
    </div>
  );
}

export function PerformanceFields(props: {
  recipe: SlurmRecipe;
  values: Record<string, string>;
  onChange: (id: string, value: string) => void;
  vramGb: number | null; // user-entered GPU VRAM; drives the n-cpu-moe hint
}): React.ReactElement {
  const { recipe, values, onChange, vramGb } = props;
  const [showAdvanced, setShowAdvanced] = useState(false);

  const params = recipe.params ?? [];
  if (params.length === 0) {
    // e.g. Custom backend - no knobs to render.
    return <></>;
  }

  const inlineParams = params.filter((p) => !p.advanced);
  const advancedParams = params.filter((p) => p.advanced);
  const supportsNCpuMoe = params.some((p) => p.id === "n_cpu_moe");

  const renderParam = (param: RecipeParam) => (
    <div
      key={param.id}
      className={param.id === "n_cpu_moe" ? "space-y-1.5 sm:col-span-2" : undefined}
    >
      <ParamField
        param={param}
        value={values[param.id] ?? ""}
        onChange={(value) => onChange(param.id, value)}
      />
      {supportsNCpuMoe && param.id === "n_cpu_moe" && (
        <NCpuMoeHint values={values} vramGb={vramGb} onChange={onChange} />
      )}
    </div>
  );

  return (
    <div className="space-y-3">
      {inlineParams.length > 0 && (
        <div className="grid gap-3 sm:grid-cols-2">
          {inlineParams.map(renderParam)}
        </div>
      )}

      {advancedParams.length > 0 && (
        <div className="space-y-3">
          <button
            type="button"
            onClick={() => setShowAdvanced((v) => !v)}
            className="text-xs font-medium text-muted-foreground hover:text-foreground"
          >
            {showAdvanced ? "Hide" : "Show"} advanced
          </button>
          {showAdvanced && (
            <div className="grid gap-3 sm:grid-cols-2">
              {advancedParams.map(renderParam)}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
