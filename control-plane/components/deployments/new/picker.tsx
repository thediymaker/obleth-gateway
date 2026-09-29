"use client";

import { useMemo, useState, useTransition } from "react";
import { Search } from "lucide-react";
import { lookupHfModelAction } from "@/app/actions";
import { Notice } from "@/components/models/ui";
import { Pill, Segmented } from "@/components/overview/ui";
import type { RecipeCard } from "@/components/recipes/recipe-card";
import { Button } from "@/components/ui/button";
import { gresCount, memToMb, partitionFits } from "@/lib/deployments-model";
import type { EngineId, HfModel } from "@/lib/hf-model";
import type { LaunchRecord } from "@/lib/launch-learning";
import type { ClusterResources } from "@/lib/obleth";
import { cn } from "@/lib/utils";

export type PickerTab = "saved" | "library" | "hf";

/** What step 1 hands to step 2. */
export interface Picked {
  recipeId: string;
  /** Starting input values (the model an engine should serve). */
  inputs?: Record<string, string>;
  name?: string;
  hf?: HfModel;
}

export const ENGINE_NAMES: Record<string, string> = { vllm: "vLLM", sglang: "SGLang", llamacpp: "llama.cpp", "llama.cpp": "llama.cpp", ollama: "Ollama" };

export function engineName(e: string | undefined): string {
  return e ? ENGINE_NAMES[e] ?? e : "—";
}

/** "nemotron-3-ultra" from "nvidia/NVIDIA-Nemotron-3-Ultra-550B-A55B-NVFP4"-style ids. */
export function nameFromRepo(model: string): string {
  const tail = model.replace(/^hf\.co\//, "").split("/").pop() ?? model;
  return tail.split(":")[0].toLowerCase().replace(/[^a-z0-9.]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 60) || "model";
}

function fitLabel(r: RecipeCard, resources: ClusterResources): { ok: boolean; text: string } | null {
  const p = r.preview;
  if (!p || !resources.partitions.length) return null;
  const fits = partitionFits(resources, { gpus: gresCount(p.gres), cpus: p.cpusPerTask ?? null, memMb: memToMb(p.mem) }).filter((f) => f.fits).map((f) => f.name);
  return fits.length ? { ok: true, text: fits.slice(0, 2).join(", ") + (fits.length > 2 ? ` +${fits.length - 2}` : "") } : { ok: false, text: "No partition" };
}

const COLS = "grid-cols-[minmax(0,1.8fr)_96px_72px_110px_minmax(0,1fr)]";

export function Picker({ recipes, launches, resources, tab, onTab, picked, onPick, onNext }: {
  recipes: RecipeCard[];
  launches: LaunchRecord[];
  resources: ClusterResources;
  tab: PickerTab;
  onTab: (t: PickerTab) => void;
  picked: Picked | null;
  onPick: (p: Picked) => void;
  onNext: () => void;
}) {
  const usable = recipes.filter((r) => r.valid && r.preview);
  const saved = usable.filter((r) => r.source === "db");
  const library = usable.filter((r) => r.source === "file" && r.preview?.kind !== "engine");
  const engines = usable.filter((r) => r.source === "file" && r.preview?.kind === "engine");
  const [query, setQuery] = useState("");
  const [engine, setEngine] = useState("all");
  const [fitsOnly, setFitsOnly] = useState(false);

  const list = tab === "saved" ? saved : library;
  const engineOptions = [...new Set(list.map((r) => r.engine).filter((e): e is string => !!e))].sort();
  const q = query.trim().toLowerCase();
  const shown = list.filter((r) => (engine === "all" || r.engine === engine) && (!fitsOnly || fitLabel(r, resources)?.ok) && (!q || `${r.name} ${r.engine} ${r.description} ${r.apiModelName} ${r.preview?.model ?? ""}`.toLowerCase().includes(q)));
  const lastRun = useMemo(() => {
    const m = new Map<string, LaunchRecord>();
    for (const l of launches) if (l.recipe_id && !m.has(l.recipe_id)) m.set(l.recipe_id, l);
    return m;
  }, [launches]);
  const current = usable.find((r) => r.id === picked?.recipeId);

  return (
    <div className="flex flex-col gap-4">
      <div role="tablist" aria-label="Start from" className="flex gap-6 border-b border-border">
        {([["saved", `Saved · ${saved.length}`], ["library", `Library · ${library.length}`], ["hf", "From Hugging Face"]] as const).map(([t, label]) => (
          <button key={t} type="button" role="tab" aria-selected={tab === t} onClick={() => { onTab(t); setEngine("all"); }} className={cn("-mb-px h-10 border-b-2 px-0.5 text-sm", tab === t ? "border-foreground font-medium text-foreground" : "border-transparent text-muted-foreground hover:text-foreground")}>{label}</button>
        ))}
      </div>

      {tab === "hf" ? (
        <FromHuggingFace engines={engines} picked={picked} onPick={onPick} onNext={onNext} />
      ) : (
        <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_360px]">
          <div className="flex min-w-0 flex-col gap-3">
            <div className="flex flex-wrap items-center gap-2">
              <label className="flex h-9 min-w-[220px] flex-1 items-center gap-2 rounded-lg border border-border bg-background px-3 text-[13px] focus-within:border-muted-foreground">
                <Search className="h-4 w-4 shrink-0 text-muted-foreground" aria-hidden />
                <input value={query} onChange={(e) => setQuery(e.target.value)} aria-label={tab === "saved" ? "Search saved recipes" : "Search the library"} placeholder="Model, engine or recipe" className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground" />
              </label>
              {engineOptions.length > 1 && <Segmented label="Engine" value={engine} onChange={setEngine} options={[{ value: "all", label: "Any engine" }, ...engineOptions.map((e) => ({ value: e, label: engineName(e) }))]} />}
              {resources.partitions.length > 0 && (
                <button type="button" aria-pressed={fitsOnly} onClick={() => setFitsOnly((x) => !x)} className={cn("inline-flex h-8 items-center rounded-full border px-3 text-[12.5px]", fitsOnly ? "border-foreground bg-secondary text-foreground" : "border-border text-muted-foreground hover:text-foreground")}>Fits a partition</button>
              )}
            </div>

            <section aria-label={tab === "saved" ? "Saved recipes" : "Library"} className="overflow-hidden rounded-xl border border-border bg-card">
              <div className="overflow-x-auto">
                <div className="min-w-[720px]">
                  <div className={cn("grid items-center gap-3.5 px-4 py-2.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground", COLS)}>
                    <span>Recipe</span><span>Engine</span><span>Size</span><span>Fits</span><span>{tab === "saved" ? "Last launched as" : "Model"}</span>
                  </div>
                  {shown.length === 0 && (
                    <p className="border-t border-border px-4 py-8 text-center text-[13px] text-muted-foreground">
                      {list.length === 0 ? (tab === "saved" ? "Nothing saved yet. Launch from the Library or Hugging Face; a launch that serves can be saved here with its settings." : "No recipes in the library.") : "Nothing matches."}
                    </p>
                  )}
                  {shown.map((r) => {
                    const on = r.id === picked?.recipeId;
                    const fit = fitLabel(r, resources);
                    const last = lastRun.get(r.id);
                    const based = r.preview?.basedOn ? usable.find((x) => x.id === r.preview?.basedOn)?.name ?? r.preview.basedOn : null;
                    return (
                      <button key={`${r.source}:${r.id}`} type="button" aria-pressed={on} onClick={() => onPick({ recipeId: r.id })} className={cn("grid w-full items-center gap-3.5 border-t border-border px-4 py-2.5 text-left text-[13px]", COLS, on ? "bg-secondary/70" : "hover:bg-secondary/30")}>
                        <span className="flex min-w-0 flex-col">
                          <span className="truncate font-medium">{r.name}</span>
                          <span className="truncate text-[11.5px] text-muted-foreground">{tab === "saved" ? (based ? `From ${based}` : "Saved") : r.description}</span>
                        </span>
                        <span className="truncate">{engineName(r.engine)}</span>
                        <span className="font-mono text-[12px] text-secondary-foreground">{r.preview?.weightsGb ? `${r.preview.weightsGb} GB` : "—"}</span>
                        <span className={cn("truncate text-[12.5px]", fit?.ok ? "text-secondary-foreground" : "text-muted-foreground")}>{fit?.text ?? "—"}</span>
                        <span className="truncate font-mono text-[12px] text-secondary-foreground">{tab === "saved" ? last?.model_name ?? <span className="font-sans text-muted-foreground">not yet</span> : r.preview?.model ?? "—"}</span>
                      </button>
                    );
                  })}
                </div>
              </div>
            </section>
            {tab === "library" && engines.length > 0 && (
              <p className="text-[12.5px] text-muted-foreground">
                A model that isn&apos;t here? <button type="button" onClick={() => onTab("hf")} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">Start from Hugging Face</button> and pick {engines.map((e) => e.name).join(", ")} for it.
              </p>
            )}
          </div>

          <aside aria-label="Selected" className="flex flex-col gap-4 rounded-xl border border-border bg-card px-5 py-5">
            {current?.preview ? (
              <Selected card={current} runs={launches.filter((l) => l.recipe_id === current.id)} onNext={onNext} />
            ) : (
              <p className="text-[13px] text-muted-foreground">Pick a recipe.</p>
            )}
          </aside>
        </div>
      )}
    </div>
  );
}

function Selected({ card, runs, onNext }: { card: RecipeCard; runs: LaunchRecord[]; onNext: () => void }) {
  const p = card.preview!;
  const served = runs.filter((r) => r.healthy_at).length;
  const values = p.inputs.filter((i) => i.type !== "path" && i.default != null && !String(i.default).includes("{{")).map((i) => `${i.label || i.name} ${i.type === "flag" ? (i.default === "true" ? "on" : "off") : i.default}`);
  return (
    <>
      <div className="flex flex-col gap-1">
        <span className="text-[17px] font-semibold leading-snug">{card.name}</span>
        {p.model && <span className="break-all font-mono text-[11.5px] text-muted-foreground">{p.model}</span>}
      </div>
      {card.description && <p className="text-[12.5px] leading-relaxed text-muted-foreground">{card.description}</p>}
      <dl className="grid grid-cols-[96px_minmax(0,1fr)] gap-x-3 gap-y-2 text-[13px]">
        <dt className="text-muted-foreground">Engine</dt><dd>{engineName(p.engine)}{p.requires ? <span className="block text-[12px] text-muted-foreground">{p.requires}</span> : null}</dd>
        {(p.nodeOptions?.length ?? 0) > 1 && <><dt className="text-muted-foreground">Runs on</dt><dd>{p.nodeOptions!.join(", ")} nodes</dd></>}
        {values.length > 0 && <><dt className="text-muted-foreground">{card.source === "db" ? "Saved values" : "Starts with"}</dt><dd className="text-secondary-foreground">{values.join(" · ")}</dd></>}
        <dt className="text-muted-foreground">Launched</dt><dd>{runs.length ? `${runs.length} time${runs.length === 1 ? "" : "s"}, ${served} served` : "Not yet"}</dd>
      </dl>
      {p.warnings.length > 0 && <p className="text-xs text-muted-foreground">{p.warnings.join(" ")}</p>}
      <div className="flex-1" />
      <Button type="button" className="h-10" onClick={onNext}>{card.source === "db" ? "Use these settings →" : "Choose where it runs →"}</Button>
    </>
  );
}

function FromHuggingFace({ engines, picked, onPick, onNext }: { engines: RecipeCard[]; picked: Picked | null; onPick: (p: Picked) => void; onNext: () => void }) {
  const [text, setText] = useState(picked?.hf?.id ?? "");
  const [model, setModel] = useState<HfModel | null>(picked?.hf ?? null);
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();
  const byEngine = new Map(engines.map((e) => [e.engine ?? "", e]));
  const chosen = model ? engines.find((e) => e.id === picked?.recipeId) : undefined;

  function look() {
    setError(null);
    start(async () => {
      const res = await lookupHfModelAction(text);
      if (!res.ok) {
        setModel(null);
        return setError(res.error);
      }
      setModel(res.model);
      const rec = res.model.engines.find((e) => e.engine === res.model.recommended && e.ok);
      if (rec) choose(res.model, rec.engine);
    });
  }

  function choose(m: HfModel, engine: EngineId) {
    const fit = m.engines.find((e) => e.engine === engine);
    const recipe = byEngine.get(engine);
    if (!fit?.ok || !recipe || !fit.model) return;
    onPick({ recipeId: recipe.id, inputs: { model: fit.model }, name: engine === "ollama" ? fit.model : nameFromRepo(m.id), hf: m });
  }

  return (
    <div className="flex flex-col gap-4">
      <form onSubmit={(e) => { e.preventDefault(); look(); }} className="flex flex-wrap items-center gap-2">
        <label className="flex h-10 min-w-[320px] flex-1 items-center gap-2 rounded-lg border border-border bg-background px-3 text-[13px] focus-within:border-muted-foreground">
          <span className="text-muted-foreground">huggingface.co/</span>
          <input value={text} onChange={(e) => setText(e.target.value)} aria-label="Hugging Face model" placeholder="org/model, e.g. Qwen/Qwen3-32B" className="min-w-0 flex-1 bg-transparent font-mono text-[12.5px] outline-none placeholder:font-sans placeholder:text-muted-foreground" />
        </label>
        <Button type="submit" variant="outline" className="h-10" disabled={pending || !text.trim()}>{pending ? "Reading…" : "Look it up"}</Button>
      </form>
      {error && <Notice strong onDismiss={() => setError(null)}>{error}</Notice>}
      {!model && !error && <p className="text-[13px] text-muted-foreground">obleth reads the repo&apos;s files and config, then lists the engines that can run it. Nothing is downloaded until the job starts.</p>}
      {model && (
        <div className="grid gap-4 xl:grid-cols-[360px_minmax(0,1fr)]">
          <section aria-label="Read from Hugging Face" className="flex flex-col gap-3 rounded-xl border border-border bg-card px-5 py-4">
            <span className="text-sm font-semibold">Read from Hugging Face</span>
            <dl className="grid grid-cols-[104px_minmax(0,1fr)] gap-x-3 gap-y-2 text-[13px]">
              <dt className="text-muted-foreground">Architecture</dt><dd className="font-mono text-[12.5px]">{model.architecture ?? model.modelType ?? "—"}</dd>
              <dt className="text-muted-foreground">Format</dt><dd>{model.format}{model.quantization ? `, ${model.quantization}` : ""}</dd>
              <dt className="text-muted-foreground">Size</dt><dd>{model.weightsGb ? `${model.weightsGb} GB` : "—"} in {model.files} files</dd>
              <dt className="text-muted-foreground">Licence</dt><dd>{model.license ?? "—"}</dd>
              <dt className="text-muted-foreground">Chat template</dt><dd>{model.chatTemplate ? "Included" : "Not found"}</dd>
              {model.gated && <><dt className="text-muted-foreground">Access</dt><dd>Gated: jobs need a Hugging Face token (Slurm connection › Cluster defaults)</dd></>}
            </dl>
          </section>
          <section aria-label="Engines that can run it" className="overflow-hidden rounded-xl border border-border bg-card">
            <div className="flex items-baseline justify-between gap-3 px-5 py-3.5"><span className="text-sm font-semibold">Engines that can run it</span><span className="text-[12px] text-muted-foreground">From what the files need</span></div>
            {model.engines.map((e) => {
              const has = byEngine.has(e.engine);
              const on = chosen?.engine === e.engine;
              return (
                <button key={e.engine} type="button" disabled={!e.ok || !has} aria-pressed={on} onClick={() => choose(model, e.engine)} className={cn("grid w-full grid-cols-[110px_minmax(0,1fr)_auto] items-center gap-3.5 border-t border-border px-5 py-3 text-left text-[13px] disabled:cursor-default", on ? "bg-secondary/70" : e.ok && has ? "hover:bg-secondary/30" : "text-muted-foreground")}>
                  <span className={cn(e.ok && "font-semibold")}>{engineName(e.engine)}</span>
                  <span className={cn(e.ok ? "text-secondary-foreground" : "text-muted-foreground")}>{has ? e.why : "No recipe for this engine in the library."}{e.ok && e.model ? <span className="block truncate font-mono text-[11.5px] text-muted-foreground">{e.model}</span> : null}</span>
                  <span>{model.recommended === e.engine && e.ok ? <Pill>Suggested</Pill> : null}</span>
                </button>
              );
            })}
            <div className="flex justify-end border-t border-border px-5 py-3">
              <Button type="button" className="h-10" disabled={!chosen} onClick={onNext}>{chosen ? `Continue with ${engineName(chosen.engine)} →` : "Pick an engine"}</Button>
            </div>
          </section>
        </div>
      )}
    </div>
  );
}
