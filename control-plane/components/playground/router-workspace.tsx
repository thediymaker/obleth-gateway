"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { Check, Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { setAutoRouterSettingsAction } from "@/app/actions";
import type { RouteExplainView, SimulateRouteRequest } from "@/lib/obleth";
import { cn } from "@/lib/utils";
import { RouteDecision } from "./route-decision";
import { ROUTER_PROMPT_MAX_LENGTH, type PendingAction, type PlaygroundSession } from "./playground";
import { Pill, SectionLabel, Segmented, SettingsPanel, SliderField, Switch } from "./ui";

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

/**
 * The Router mode of the Playground: paste a prompt, tune the auto-router's
 * weights, and see a live A/B between what the *saved* (live) weights would
 * pick and what the *edited* weights on screen would pick. Sliders only
 * simulate — `Apply to gateway`, behind a confirmation that lists every
 * change, is the sole path that writes settings.
 */
export function RouterWorkspace({ session, update, settingsOpen = true, onOpenSession }: {
  session: PlaygroundSession; update: (patch: Partial<PlaygroundSession>) => void;
  settingsOpen?: boolean;
  onOpenSession?: (seed: Partial<PlaygroundSession>, action?: PendingAction) => void;
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

  // Default ON: the whole point of the playground is predicting what serving
  // would do, and serving classifies with the brain when one is configured.
  // The endpoint falls back to heuristics (and says so via tag_source) when
  // the classifier is off, so leaving this on is always safe.
  const [classifyLive, setClassifyLive] = useState(true);

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
  const [confirming, setConfirming] = useState(false);

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
    if (classifyLive) shared.classify = true;
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
    classifyLive, capacityWeight, costWeight, tagWeight, softCap, temperature, difficultyEnabled,
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

  function resetToLive() {
    if (!baseline) return;
    setCapacityWeight(baseline.weights.capacity);
    setCostWeight(baseline.weights.cost);
    setTagWeight(baseline.weights.tag);
    setSoftCap(baseline.weights.soft_cap);
    setTemperature(baseline.temperature);
    setDifficultyEnabled(baseline.weights.difficulty_enabled);
  }

  const current = edited ?? baseline;
  // What Apply would change, spelled out for the confirmation.
  const changes = !baseline ? [] : [
    { label: "Capacity weight", from: baseline.weights.capacity.toFixed(2), to: capacityWeight.toFixed(2) },
    { label: "Cost weight", from: baseline.weights.cost.toFixed(2), to: costWeight.toFixed(2) },
    { label: "Tag weight", from: baseline.weights.tag.toFixed(2), to: tagWeight.toFixed(2) },
    { label: "Temperature", from: baseline.temperature.toFixed(2), to: temperature.toFixed(2) },
    { label: "Default soft cap", from: String(baseline.weights.soft_cap), to: String(Math.round(softCap)) },
    { label: "Difficulty tiering", from: baseline.weights.difficulty_enabled ? "on" : "off", to: difficultyEnabled ? "on" : "off" },
  ].filter((c) => c.from !== c.to);
  /** The live value to strike through beside a slider, only when the draft differs. */
  const liveOf = (live: number | undefined, edited: number, digits = 2) =>
    live !== undefined && Math.abs(live - edited) > EPSILON ? live.toFixed(digits) : undefined;
  const needs = [
    { label: "Function calling", on: needsFunctionCalling, key: "routerNeedsFunctionCalling" as const },
    { label: "Forced tool choice", on: needsToolChoice, key: "routerNeedsToolChoice" as const },
    { label: "Response schema", on: needsResponseSchema, key: "routerNeedsResponseSchema" as const },
  ];
  const openInChat = (model: string) => onOpenSession?.({ title: prompt.trim().slice(0, 60) || "Routed chat", mode: "compare", models: [model], chatDraft: prompt });

  return (
    <div className="relative flex h-full min-h-0">
      <section aria-label="Routing simulation" className="min-w-0 flex-1 overflow-y-auto px-4 py-5 md:px-6">
        <div className="mx-auto max-w-4xl space-y-4">
          <div className="rounded-2xl border border-border bg-card focus-within:border-muted-foreground/50">
            <SectionLabel htmlFor="router-prompt" className="block px-4 pt-3">Prompt to route</SectionLabel>
            <textarea
              id="router-prompt"
              aria-label="Prompt"
              rows={3}
              maxLength={ROUTER_PROMPT_MAX_LENGTH}
              value={prompt}
              onChange={(e) => onPromptChange(e.target.value)}
              placeholder="Paste or write the request you want to see routed…"
              className="block w-full resize-y bg-transparent px-4 pb-2 pt-1.5 text-sm leading-relaxed outline-none placeholder:text-muted-foreground"
            />
            <div className="flex flex-wrap items-center gap-1.5 border-t border-border px-3 pb-3 pt-2.5">
              <span className="mr-1 text-xs text-muted-foreground">Request needs</span>
              {needs.map((n) => (
                <button key={n.key} type="button" aria-pressed={n.on} onClick={() => update({ [n.key]: !n.on })}
                  className={cn("inline-flex h-[30px] items-center gap-1.5 rounded-lg border px-2.5 text-[12.5px]", n.on ? "border-muted-foreground/40 bg-secondary text-foreground" : "border-border text-muted-foreground hover:text-foreground")}>
                  <span className={cn("flex h-3.5 w-3.5 items-center justify-center rounded", n.on ? "bg-foreground text-background" : "border-[1.5px] border-muted-foreground/60")}>{n.on && <Check className="h-2.5 w-2.5" strokeWidth={3} />}</span>
                  {n.label}
                </button>
              ))}
              <span className="ml-auto flex items-center gap-1.5 text-xs text-muted-foreground">
                {busy && <Loader2 className="h-3.5 w-3.5 animate-spin" aria-hidden="true" />}
                {busy ? "Simulating…" : "Updates as you type"}
              </span>
            </div>
          </div>

          {error && <p role="alert" className="rounded-xl border border-border bg-secondary/50 px-3.5 py-2.5 text-sm">{error}</p>}
          {!current && !busy && !error && (
            <p className="rounded-xl border border-dashed border-border p-6 text-center text-sm text-muted-foreground">Enter a prompt above to see how the auto-router would score it.</p>
          )}
          {current && <RouteDecision explain={current} baseline={dirty ? baseline ?? undefined : undefined} onOpenInChat={onOpenSession && prompt.trim() ? openInChat : undefined} />}
        </div>
      </section>

      {settingsOpen && (
        <SettingsPanel
          title="Router weights"
          label="Router settings"
          action={weightStatus === "edited" ? <Pill inverted>Draft</Pill> : <span className="text-xs text-muted-foreground">{weightStatus === "matches" ? "Matches live" : "Live weights unavailable"}</span>}
          footer={
            <>
              <div className="flex gap-2">
                <Button variant="outline" className="flex-1" disabled={!dirty || applying} onClick={resetToLive}>Reset to live</Button>
                <Button className="flex-1" disabled={applying || !dirty} onClick={() => setConfirming(true)}>{applying ? "Applying…" : "Apply to gateway…"}</Button>
              </div>
              {applyStatus
                ? <p role="status" className="text-[11.5px] text-secondary-foreground">{applyStatus}</p>
                : <p className="text-[11.5px] leading-snug text-muted-foreground">Try weights here without touching live traffic. Applying changes routing for everything sent to auto.</p>}
            </>
          }
        >
          <section className="space-y-4">
            <SliderField id="router-capacity-weight" label="Capacity" value={capacityWeight} onChange={setCapacityWeight} min={0} max={1} step={0.05} live={liveOf(baseline?.weights.capacity, capacityWeight)} hint="Idle capacity's influence on the ranking." />
            <SliderField id="router-cost-weight" label="Cost" value={costWeight} onChange={setCostWeight} min={0} max={1} step={0.05} live={liveOf(baseline?.weights.cost, costWeight)} hint="Price's influence on the ranking." />
            <SliderField id="router-tag-weight" label="Tag match" value={tagWeight} onChange={setTagWeight} min={0} max={1} step={0.05} live={liveOf(baseline?.weights.tag, tagWeight)} hint="Topic match's influence on the ranking." />
            <SliderField id="router-temperature" label="Temperature" value={temperature} onChange={setTemperature} min={0} max={2} step={0.05} live={liveOf(baseline?.temperature, temperature)} hint="0 always picks the top scorer; higher spreads traffic across close scorers." />
            <SliderField id="router-soft-cap" label="Default soft cap" value={softCap} display={String(Math.round(softCap))} onChange={setSoftCap} min={1} max={64} step={1} live={liveOf(baseline?.weights.soft_cap, softCap, 0)} hint="Assumed concurrency ceiling for models with no explicit limit." />
          </section>

          <section className="space-y-3 border-t border-border pt-4">
            <div className="flex items-center justify-between gap-3 text-[13px]">
              <span>Difficulty tiering{baseline && difficultyEnabled !== baseline.weights.difficulty_enabled && <span className="ml-1.5 text-[11.5px] text-muted-foreground">live: {baseline.weights.difficulty_enabled ? "on" : "off"}</span>}</span>
              <Switch label="Difficulty tiering" checked={difficultyEnabled} onChange={setDifficultyEnabled} />
            </div>
            <div className="flex items-center justify-between gap-3 text-[13px]">
              <span title="Derive tags and difficulty with the live classifier model (one small model call per simulation), exactly as serving does. Off, a keyword heuristic is used.">
                Live classifier <span className="block text-[11.5px] text-muted-foreground">One small model call per simulation</span>
              </span>
              <Switch label="Live classifier" checked={classifyLive} onChange={setClassifyLive} />
            </div>
          </section>

          <section className="space-y-3 border-t border-border pt-4">
            <SectionLabel>Request</SectionLabel>
            <Segmented
              label="Effort"
              value={effort || "default"}
              onChange={(v) => update({ routerEffort: v === "default" ? undefined : v })}
              options={[{ value: "default", label: "Default" }, { value: "low", label: "Low" }, { value: "medium", label: "Medium" }, { value: "high", label: "High" }]}
            />
            <div className="grid grid-cols-2 gap-2">
              <Input aria-label="Max output tokens" type="number" min={1} max={131072} placeholder="Max tokens" value={maxTokens ?? ""}
                onChange={(e) => { const n = e.target.valueAsNumber; if (!e.target.value || (Number.isInteger(n) && n >= 1 && n <= 131072)) update({ routerMaxTokens: e.target.value ? n : undefined }); }} />
              <Input aria-label="Tenant ID" maxLength={200} placeholder="Tenant (optional)" value={tenantId} onChange={(e) => update({ routerTenantId: e.target.value })} />
            </div>
            <p className="text-[11.5px] text-muted-foreground">A tenant applies its own route rules to the simulation.</p>
          </section>
        </SettingsPanel>
      )}

      <Dialog open={confirming} onOpenChange={setConfirming}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Apply these weights to the gateway?</DialogTitle>
            <DialogDescription>Every request sent to auto is routed with the new weights as soon as you apply them.</DialogDescription>
          </DialogHeader>
          <div className="overflow-hidden rounded-lg border border-border text-sm">
            {changes.map((c) => (
              <div key={c.label} className="flex items-center justify-between gap-3 border-b border-border px-3 py-2 last:border-b-0">
                <span>{c.label}</span>
                <span className="font-mono text-xs"><span className="text-muted-foreground line-through">{c.from}</span> → {c.to}</span>
              </div>
            ))}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setConfirming(false)}>Cancel</Button>
            <Button onClick={() => { setConfirming(false); void applyToGateway(); }}>Apply to gateway</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
