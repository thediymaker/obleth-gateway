"use client";

import { useState } from "react";
import Link from "next/link";
import { SettingsCard } from "@/components/access/ui";
import { Glyph } from "@/components/deployments/ui";
import { SelectField, Setting, Switch, TextField } from "@/components/models/fields";
import { Pill } from "@/components/overview/ui";
import type { AutoRouterSettingsView, ModelRoute, RouterReadinessView } from "@/lib/obleth";
import { ROUTER_PROFILES, routerProfileOf, routerValues, type RouterValues } from "@/lib/settings-model";
import { cn } from "@/lib/utils";

const WEIGHTS: { key: "capacity_weight" | "cost_weight" | "tag_weight"; label: string; means: string }[] = [
  { key: "capacity_weight", label: "Free capacity", means: "prefers a model with open slots" },
  { key: "cost_weight", label: "Price", means: "prefers cheaper tokens" },
  { key: "tag_weight", label: "Task fit", means: "prefers models tagged for the kind of question" },
];

const TIERS = [
  { value: "hybrid", label: "From tags and price" },
  { value: "derived", label: "From price only" },
  { value: "declared", label: "Only what models declare" },
];

/** How the `auto` model picks a real one for each request. */
export function RoutingSection({ settings, models, readiness }: { settings: AutoRouterSettingsView; models: ModelRoute[]; readiness: RouterReadinessView | null }) {
  const [v, setV] = useState<RouterValues>(routerValues(settings));
  const [classifierOn, setClassifierOn] = useState(settings.classifier_enabled);
  const [classifier, setClassifier] = useState(settings.classifier_model ?? "");
  const [anthropic, setAnthropic] = useState(settings.messages_default_model ?? "");
  const [advanced, setAdvanced] = useState(false);
  const profile = routerProfileOf(v);
  const set = <K extends keyof RouterValues>(k: K, value: RouterValues[K]) => setV((x) => ({ ...x, [k]: value }));
  const names = models.filter((m) => m.model_name !== "auto").map((m) => m.model_name).sort();
  const warns = (readiness?.findings ?? []).filter((f) => f.severity === "warn");
  const infos = (readiness?.findings ?? []).filter((f) => f.severity !== "warn");

  return (
    <SettingsCard
      id="routing"
      title="Routing"
      description={<>How the <span className="font-mono">auto</span> model picks a real one for each request.</>}
      action={
        <div className="flex flex-wrap items-center gap-2">
          {readiness && (warns.length ? <Pill inverted>{warns.length} to fix</Pill> : <Pill><Glyph glyph="on" className="h-[7px] w-[7px]" />Ready · {readiness.pool_size} models in the pool</Pill>)}
          <Link href="/playground" className="inline-flex h-[22px] items-center rounded-full border border-border px-2 text-[11.5px] text-secondary-foreground hover:text-foreground">Try it in the Playground ›</Link>
        </div>
      }
    >
      {[...warns, ...infos].map((f) => (
        <div key={f.code} className="border-t border-border px-[18px] py-2.5 text-[12.5px] first:border-t-0">
          <span className={cn("font-medium", f.severity !== "warn" && "text-secondary-foreground")}>{f.title}</span>
          <span className="text-muted-foreground"> · {f.detail}</span>
        </div>
      ))}
      <input type="hidden" name="routing.capacity_weight" value={String(v.capacity_weight)} />
      <input type="hidden" name="routing.cost_weight" value={String(v.cost_weight)} />
      <input type="hidden" name="routing.tag_weight" value={String(v.tag_weight)} />
      <input type="hidden" name="routing.temperature" value={String(v.temperature)} />
      <input type="hidden" name="routing.soft_cap" value={String(v.soft_cap)} />
      <input type="hidden" name="routing.tier_source" value={v.tier_source} />
      <Setting id="set-router-profile" label="Profile" hint="Sets the weights below; change any after." fields={["routing.capacity_weight", "routing.cost_weight", "routing.tag_weight", "routing.temperature", "routing.soft_cap", "routing.difficulty", "routing.tier_source"]}>
        <div className="grid gap-2 lg:grid-cols-4">
          {ROUTER_PROFILES.map((p) => (
            <button key={p.key} type="button" aria-pressed={profile === p.key} onClick={() => setV(p.values)} className={cn("flex flex-col gap-0.5 rounded-lg border px-3 py-2 text-left", profile === p.key ? "border-foreground bg-secondary" : "border-border hover:border-muted-foreground/60")}>
              <span className="text-[12.5px] font-medium">{p.label}</span>
              <span className="text-[11.5px] leading-snug text-muted-foreground">{p.blurb}</span>
            </button>
          ))}
          <button type="button" aria-pressed={profile === "custom"} onClick={() => setAdvanced(true)} className={cn("flex flex-col gap-0.5 rounded-lg border px-3 py-2 text-left", profile === "custom" ? "border-foreground bg-secondary" : "border-border hover:border-muted-foreground/60")}>
            <span className="text-[12.5px] font-medium">Custom</span>
            <span className="text-[11.5px] leading-snug text-muted-foreground">Your own weights.</span>
          </button>
        </div>
      </Setting>
      <Setting id="set-router-weights" label="What matters" hint="How much each counts when two models could take a request." fields={["routing.capacity_weight", "routing.cost_weight", "routing.tag_weight"]}>
        {WEIGHTS.map((w) => (
          <label key={w.key} className="grid grid-cols-[100px_minmax(0,260px)_44px_minmax(0,1fr)] items-center gap-3 text-[12.5px]">
            <span>{w.label}</span>
            <input type="range" min={0} max={1} step={0.05} value={v[w.key]} onChange={(e) => set(w.key, Number(e.target.value))} aria-label={w.label} className="accent-foreground" />
            <span className="font-mono">{v[w.key].toFixed(2)}</span>
            <span className="text-muted-foreground">{w.means}</span>
          </label>
        ))}
      </Setting>
      <Setting id="set-classifier" label="Classifier" hint="A small model that reads each request and names its kind (coding, math…). Off, routing guesses from the text." fields={["routing.classifier_enabled", "routing.classifier_model", "routing.classifier_timeout_ms"]}>
        <div className="flex flex-wrap items-center gap-2.5">
          <Switch name="routing.classifier_enabled" label="Use a classifier" checked={classifierOn} onChange={setClassifierOn} />
          <div className="w-64"><SelectField name="routing.classifier_model" label="Classifier model" value={classifier} onChange={setClassifier} options={[{ value: "", label: "None" }, ...names.map((n) => ({ value: n, label: n }))]} /></div>
          <span className="text-xs text-muted-foreground">gives up after</span>
          <TextField name="routing.classifier_timeout_ms" label="Classifier timeout" inputMode="numeric" defaultValue={settings.classifier_timeout_ms} mono className="w-24" />
          <span className="text-xs text-muted-foreground">ms</span>
        </div>
      </Setting>
      <Setting id="set-difficulty" label="Hard questions" hint="Send harder questions to stronger models." fields={["routing.difficulty", "routing.tier_source"]}>
        <div className="flex flex-wrap items-center gap-2.5">
          <Switch name="routing.difficulty" label="Send hard questions to stronger models" checked={v.difficulty} onChange={(on) => set("difficulty", on)} />
          {v.difficulty && <div className="w-60"><SelectField label="How strength is judged" value={v.tier_source} onChange={(t) => set("tier_source", t as RouterValues["tier_source"])} options={TIERS} /></div>}
        </div>
      </Setting>
      <Setting id="set-anthropic" label="Anthropic clients" hint="The model a /v1/messages request gets when it names a Claude model obleth doesn't have." fields={["routing.messages_default_model"]} was={{ field: "routing.messages_default_model" }}>
        <div className="w-64"><SelectField name="routing.messages_default_model" label="Default for Anthropic clients" value={anthropic} onChange={setAnthropic} options={[{ value: "", label: "None: refuse the request" }, { value: "auto", label: "auto" }, ...names.map((n) => ({ value: n, label: n }))]} /></div>
      </Setting>
      <Setting label="Advanced" hint="Randomness when scores are close, and how many requests a model takes before others are tried." fields={["routing.temperature", "routing.soft_cap"]}>
        {!advanced ? (
          <button type="button" onClick={() => setAdvanced(true)} className="self-start text-[12.5px] text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">Show 2 more settings</button>
        ) : (
          <div className="flex flex-col gap-2">
            <label className="grid grid-cols-[100px_minmax(0,260px)_80px] items-center gap-3 text-[12.5px]">
              <span>Randomness</span>
              <input type="range" min={0} max={2} step={0.1} value={v.temperature} onChange={(e) => set("temperature", Number(e.target.value))} aria-label="Randomness" className="accent-foreground" />
              <span className="font-mono">{v.temperature === 0 ? "none" : v.temperature.toFixed(1)}</span>
            </label>
            <label className="flex items-center gap-2 text-[12.5px]">
              <span className="w-[100px]">Soft cap</span>
              <input value={v.soft_cap} onChange={(e) => set("soft_cap", Math.max(1, Math.round(Number(e.target.value) || 1)))} inputMode="numeric" aria-label="Soft cap" className="h-9 w-20 rounded-md border border-input bg-background px-3 font-mono text-[12.5px]" />
              <span className="text-muted-foreground">requests in flight on a model before it scores lower</span>
            </label>
          </div>
        )}
      </Setting>
    </SettingsCard>
  );
}
