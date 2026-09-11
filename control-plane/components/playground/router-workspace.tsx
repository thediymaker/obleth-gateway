"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { Loader2, Wand2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { setAutoRouterSettingsAction } from "@/app/actions";
import type { RouteExplainView, SimulateRouteRequest } from "@/lib/obleth";
import { cn } from "@/lib/utils";
import { RouteExplainPanel } from "./route-explain";
import { ROUTER_PROMPT_MAX_LENGTH, type PlaygroundSession } from "./playground";

const DEBOUNCE_MS = 300;
const EPSILON = 1e-6;

async function postSimulate(body: SimulateRouteRequest, signal: AbortSignal): Promise<RouteExplainView> {
  const res = await fetch("/api/live/router/simulate", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
    signal,
  });
  if (!res.ok) {
    const payload = await res.json().catch(() => null);
    throw new Error(payload?.error || `Simulation failed (HTTP ${res.status}).`);
  }
  return (await res.json()) as RouteExplainView;
}

function WeightSlider({ id, label, hint, value, onChange, min, max, step, valueLabel }: {
  id: string; label: string; hint: string; value: number; onChange: (value: number) => void;
  min: number; max: number; step: number; valueLabel?: string;
}) {
  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between gap-2">
        <Label htmlFor={id}>{label}</Label>
        <span className="text-xs font-medium tabular-nums text-foreground">{valueLabel ?? value.toFixed(2)}</span>
      </div>
      <input id={id} type="range" min={min} max={max} step={step} value={value} onChange={(e) => onChange(Number(e.target.value))}
        className="h-1.5 w-full cursor-pointer appearance-none rounded-full bg-muted accent-foreground" />
      <p className="text-[11px] text-muted-foreground">{hint}</p>
    </div>
  );
}

/**
 * The Router mode of the Playground: paste a prompt, tune the auto-router's
 * weights, and see a live A/B between what the *saved* (live) weights would
 * pick and what the *edited* weights on screen would pick. Sliders only
 * simulate — `Apply to gateway` is the sole path that writes settings.
 */
export function RouterWorkspace({ session, update }: {
  session: PlaygroundSession; update: (patch: Partial<PlaygroundSession>) => void;
}) {
  // Kept on the session, not local state, so the form survives switching to
  // Chat mode and back — that toggle remounts this component. (Weights are
  // the deliberate exception; see the schema comment in playground.tsx.)
  const prompt = session.routerPrompt ?? "";
  const tenantId = session.routerTenantId ?? "";
  const effort: "" | "low" | "medium" | "high" = session.routerEffort ?? "";
  const maxTokens = session.routerMaxTokens;
  const needsFunctionCalling = session.routerNeedsFunctionCalling ?? false;
  const needsToolChoice = session.routerNeedsToolChoice ?? false;
  const needsResponseSchema = session.routerNeedsResponseSchema ?? false;

  const [capacityWeight, setCapacityWeight] = useState(0.6);
  const [costWeight, setCostWeight] = useState(0.4);
  const [tagWeight, setTagWeight] = useState(0.5);
  const [softCap, setSoftCap] = useState(8);
  const [temperature, setTemperature] = useState(0);
  const [difficultyEnabled, setDifficultyEnabled] = useState(false);

  const [baseline, setBaseline] = useState<RouteExplainView | null>(null);
  const [edited, setEdited] = useState<RouteExplainView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [applying, setApplying] = useState(false);
  const [applyStatus, setApplyStatus] = useState<string | null>(null);

  // Seed the sliders from the saved weights the first time a baseline
  // response arrives, so the panel opens showing the fleet's real settings
  // rather than an arbitrary guess. A ref (not state) keeps this out of
  // `run`'s dependencies — otherwise every completed run would recreate
  // `run`, re-arm the debounce effect below, and refetch forever.
  const seeded = useRef(false);
  const controller = useRef<AbortController | null>(null);

  const run = useCallback(async () => {
    controller.current?.abort();
    const ac = new AbortController();
    controller.current = ac;
    setBusy(true);
    setError(null);
    // One draw shared by both calls of this A/B: at temperature > 0 the
    // router samples, so two independent draws would make sampling noise
    // look like the effect of the weight edit. Pinning `uniform` here, then
    // spreading it into both request bodies below, is what guarantees the
    // two calls agree.
    const uniform = Math.random();
    const shared: SimulateRouteRequest = { uniform };
    if (prompt.trim()) shared.prompt = prompt;
    if (tenantId.trim()) shared.tenant_id = tenantId.trim();
    if (effort) shared.effort = effort;
    if (maxTokens !== undefined) shared.max_tokens = maxTokens;
    if (needsFunctionCalling) shared.needs_function_calling = true;
    if (needsToolChoice) shared.needs_tool_choice = true;
    if (needsResponseSchema) shared.needs_response_schema = true;
    const editedBody: SimulateRouteRequest = {
      ...shared,
      capacity_weight: capacityWeight,
      cost_weight: costWeight,
      tag_weight: tagWeight,
      default_soft_cap: Math.round(softCap),
      temperature,
      difficulty_enabled: difficultyEnabled,
    };
    // Reports whether this run actually landed a fresh baseline/edited pair,
    // so callers (Apply to gateway) can state what happened instead of
    // assuming success — `baseline`/`dirty` read here would be stale, since
    // the setState calls below don't retroactively update this closure.
    let ok = true;
    try {
      const [b, e] = await Promise.all([postSimulate(shared, ac.signal), postSimulate(editedBody, ac.signal)]);
      if (ac.signal.aborted) return true;
      setBaseline(b);
      setEdited(e);
      if (!seeded.current) {
        seeded.current = true;
        setCapacityWeight(b.weights.capacity);
        setCostWeight(b.weights.cost);
        setTagWeight(b.weights.tag);
        setSoftCap(b.weights.soft_cap);
        setTemperature(b.temperature);
        setDifficultyEnabled(b.weights.difficulty_enabled);
      }
    } catch (err) {
      if ((err as Error).name !== "AbortError") { setError((err as Error).message || "Unable to reach the gateway."); ok = false; }
    } finally {
      if (!ac.signal.aborted) setBusy(false);
    }
    return ok;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    prompt, tenantId, effort, maxTokens, needsFunctionCalling, needsToolChoice, needsResponseSchema,
    capacityWeight, costWeight, tagWeight, softCap, temperature, difficultyEnabled,
  ]);

  // Debounce every input, including range-input drags (which fire per frame)
  // — each run is two HTTP calls, so an undebounced slider would flood the
  // gateway.
  useEffect(() => {
    const handle = setTimeout(() => { void run(); }, DEBOUNCE_MS);
    return () => clearTimeout(handle);
  }, [run]);
  useEffect(() => () => controller.current?.abort(), []);

  const dirty = !!baseline && (
    Math.abs(capacityWeight - baseline.weights.capacity) > EPSILON ||
    Math.abs(costWeight - baseline.weights.cost) > EPSILON ||
    Math.abs(tagWeight - baseline.weights.tag) > EPSILON ||
    Math.abs(softCap - baseline.weights.soft_cap) > EPSILON ||
    Math.abs(temperature - baseline.temperature) > EPSILON ||
    difficultyEnabled !== baseline.weights.difficulty_enabled
  );
  // Three states, not two: without a baseline (nothing fetched yet, or the
  // last fetch failed) there is nothing to claim parity with, so "matches
  // live" would be a statement about settings the panel never read.
  const weightStatus: "matches" | "edited" | "unavailable" = !baseline ? "unavailable" : dirty ? "edited" : "matches";

  async function applyToGateway() {
    setApplying(true);
    setApplyStatus(null);
    const result = await setAutoRouterSettingsAction({
      capacity_weight: capacityWeight,
      cost_weight: costWeight,
      tag_weight: tagWeight,
      default_soft_cap: Math.round(softCap),
      temperature,
      difficulty_enabled: difficultyEnabled,
    });
    setApplying(false);
    if (!result.ok) { setApplyStatus(result.error); return; }
    // Say what actually happened, not what was hoped for: `run()`'s return
    // reflects whether the post-Apply refetch itself succeeded, since the
    // `baseline`/`dirty` in this closure are snapshots from before the
    // refetch and can't be trusted to describe its outcome.
    const refetched = await run();
    setApplyStatus(
      refetched
        ? "Applied. The live baseline now matches these weights."
        : "Settings were applied, but refreshing the baseline afterward failed — reload to confirm.",
    );
  }

  function onPromptChange(value: string) {
    const patch: Partial<PlaygroundSession> = { routerPrompt: value };
    if (session.title === "Untitled session" && value.trim()) patch.title = value.trim().slice(0, 60);
    update(patch);
  }

  const current = edited ?? baseline;

  return (
    <div className="flex h-full min-h-0 flex-col overflow-y-auto">
      <div className="space-y-3 border-b border-border p-4">
        <label className="block text-xs font-medium">
          Prompt
          <textarea
            aria-label="Prompt"
            rows={3}
            maxLength={ROUTER_PROMPT_MAX_LENGTH}
            value={prompt}
            onChange={(e) => onPromptChange(e.target.value)}
            placeholder="Paste or write the request you want to see routed…"
            className="mt-1 w-full resize-y rounded-md border border-border bg-background p-2 text-sm outline-none focus:ring-1 focus:ring-ring"
          />
        </label>
        <div className="grid gap-3 sm:grid-cols-4">
          <label className="text-xs font-medium">
            Tenant ID
            <Input aria-label="Tenant ID" className="mt-1" maxLength={200} placeholder="Optional" value={tenantId} onChange={(e) => update({ routerTenantId: e.target.value })} />
          </label>
          <label className="text-xs font-medium">
            Effort
            <select
              aria-label="Effort"
              className="mt-1 flex h-9 w-full rounded-md border border-input bg-background px-3 py-1 text-sm"
              value={effort}
              onChange={(e) => update({ routerEffort: e.target.value ? (e.target.value as "low" | "medium" | "high") : undefined })}
            >
              <option value="">Default</option>
              <option value="low">Low</option>
              <option value="medium">Medium</option>
              <option value="high">High</option>
            </select>
          </label>
          <label className="text-xs font-medium">
            Max output tokens
            <Input aria-label="Max output tokens" className="mt-1" type="number" min={1} max={131072} placeholder="Unset" value={maxTokens ?? ""} onChange={(e) => {
              const n = e.target.valueAsNumber;
              if (!e.target.value || (Number.isInteger(n) && n >= 1 && n <= 131072)) update({ routerMaxTokens: e.target.value ? n : undefined });
            }} />
          </label>
          <div className="flex flex-col justify-end gap-1 text-xs">
            <label className="flex items-center gap-2"><input type="checkbox" checked={needsFunctionCalling} onChange={(e) => update({ routerNeedsFunctionCalling: e.target.checked })} className="h-3.5 w-3.5" />Function calling</label>
            <label className="flex items-center gap-2"><input type="checkbox" checked={needsToolChoice} onChange={(e) => update({ routerNeedsToolChoice: e.target.checked })} className="h-3.5 w-3.5" />Forced tool choice</label>
            <label className="flex items-center gap-2"><input type="checkbox" checked={needsResponseSchema} onChange={(e) => update({ routerNeedsResponseSchema: e.target.checked })} className="h-3.5 w-3.5" />Response schema</label>
          </div>
        </div>
      </div>

      <div className="space-y-3 border-b border-border bg-secondary/10 p-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <Wand2 className="h-4 w-4 text-violet-500" aria-hidden="true" />
            <span className="text-sm font-medium">Weights</span>
            <span
              className={cn(
                "rounded-full px-2 py-0.5 text-xs font-medium",
                weightStatus === "edited" && "bg-amber-500/15 text-amber-600 dark:text-amber-400",
                weightStatus === "matches" && "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400",
                weightStatus === "unavailable" && "bg-muted text-muted-foreground",
              )}
            >
              {weightStatus === "edited" ? "✎ edited" : weightStatus === "matches" ? "matches live" : "live weights unavailable"}
            </span>
            {busy && <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" aria-hidden="true" />}
          </div>
          <Button size="sm" onClick={() => void applyToGateway()} disabled={applying || !dirty}>
            {applying ? "Applying…" : "Apply to gateway"}
          </Button>
        </div>
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          <WeightSlider id="router-capacity-weight" label="Capacity" hint="Idle capacity's influence on the ranking." value={capacityWeight} onChange={setCapacityWeight} min={0} max={1} step={0.05} />
          <WeightSlider id="router-cost-weight" label="Cost" hint="Price's influence on the ranking." value={costWeight} onChange={setCostWeight} min={0} max={1} step={0.05} />
          <WeightSlider id="router-tag-weight" label="Tag" hint="Topic match's influence on the ranking." value={tagWeight} onChange={setTagWeight} min={0} max={1} step={0.05} />
        </div>
        <div className="grid gap-4 sm:grid-cols-2">
          <WeightSlider id="router-temperature" label="Temperature" hint="0 always picks the top scorer; higher spreads traffic across close scorers." value={temperature} onChange={setTemperature} min={0} max={2} step={0.1} valueLabel={temperature === 0 ? "Deterministic" : temperature.toFixed(1)} />
          <WeightSlider id="router-soft-cap" label="Default soft cap" hint="Assumed concurrency ceiling for models with no explicit limit." value={softCap} onChange={setSoftCap} min={1} max={64} step={1} valueLabel={String(Math.round(softCap))} />
        </div>
        <label className="flex items-center gap-2 text-xs">
          <input type="checkbox" checked={difficultyEnabled} onChange={(e) => setDifficultyEnabled(e.target.checked)} className="h-3.5 w-3.5" />
          Difficulty tiering
        </label>
        {applyStatus && <p role="status" className="text-xs text-muted-foreground">{applyStatus}</p>}
      </div>

      <div className="flex-1 p-4">
        {error && <p role="alert" className="mb-3 text-xs text-destructive">{error}</p>}
        {!current && !busy && !error && (
          <p className="text-sm text-muted-foreground">Enter a prompt above to see how the auto-router would score it.</p>
        )}
        {current && <RouteExplainPanel explain={current} baseline={baseline ?? undefined} />}
      </div>
    </div>
  );
}
