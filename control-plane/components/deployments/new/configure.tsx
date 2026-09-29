"use client";

import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import Link from "next/link";
import { X } from "lucide-react";
import { Pill, Segmented } from "@/components/overview/ui";
import type { RecipeCard } from "@/components/recipes/recipe-card";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";
import { engineName } from "@/components/deployments/new/picker";
import { changedPaths, formToYaml, getField, setField, yamlToForm, type DeployForm, type FieldPath, type Problem, type Source } from "@/lib/deploy-form";
import { formatTimeLimit, gresCount, mbLabel, memToMb, parseTimeLimit, partitionFits, walltimeChoices, walltimeLabel, type PartitionFit } from "@/lib/deployments-model";
import type { Learned, Suggestion } from "@/lib/launch-learning";
import type { ClusterResources } from "@/lib/obleth";
import { inputDefault, inputSource, type ClusterValues, type RecipeInput } from "@/lib/recipe-inputs";
import { cn } from "@/lib/utils";

const SOURCE_LABEL: Record<Source, string> = { recipe: "Recipe", cluster: "Cluster default", learned: "Learned", changed: "Changed", default: "Slurm default" };

const input = "h-9 rounded-lg border border-input bg-background px-3 text-[13px] outline-none focus-visible:ring-1 focus-visible:ring-ring";

function SourceTag({ source, saved }: { source: Source; saved: boolean }) {
  const label = source === "recipe" && saved ? "Saved" : SOURCE_LABEL[source];
  return <span className={cn("whitespace-nowrap pt-2 text-right text-[11.5px]", source === "changed" ? "font-medium text-foreground" : source === "learned" ? "text-foreground underline decoration-dotted underline-offset-[3px]" : "text-muted-foreground")}>{label}</span>;
}

function Row({ label, hint, source, saved, problem, children }: { label: string; hint?: ReactNode; source?: Source; saved: boolean; problem?: string; children: ReactNode }) {
  return (
    <div className="grid gap-x-4 gap-y-1.5 border-t border-border px-5 py-3 md:grid-cols-[200px_minmax(0,1fr)_104px]">
      <div className="pt-1.5">
        <div className="text-[13.5px]">{label}</div>
        {hint && <div className="mt-0.5 text-[12px] leading-snug text-muted-foreground">{hint}</div>}
      </div>
      <div className="flex min-w-0 flex-col gap-1.5">
        {children}
        {problem && <p className="text-[12px] font-medium text-foreground">! {problem}</p>}
      </div>
      {source ? <SourceTag source={source} saved={saved} /> : <span />}
    </div>
  );
}

function Section({ title, note, children, label }: { title: string; note?: ReactNode; children: ReactNode; label?: string }) {
  return (
    <section aria-label={label ?? title} className="overflow-hidden rounded-xl border border-border bg-card">
      <div className="flex flex-wrap items-baseline justify-between gap-2 px-5 py-3.5"><span className="text-sm font-semibold">{title}</span>{note && <span className="text-[12px] text-muted-foreground">{note}</span>}</div>
      {children}
    </section>
  );
}

/** A suggestion beside its field, with the evidence and a one-click fix. */
function Hint({ s, onApply }: { s: Suggestion; onApply: (s: Suggestion) => void }) {
  return (
    <p className="flex flex-wrap items-baseline gap-x-2 border-l-2 border-dotted border-muted-foreground/70 pl-2.5 text-[12px] leading-snug text-secondary-foreground">
      <span>{s.evidence}</span>
      {s.value != null && <button type="button" onClick={() => onApply(s)} className="text-foreground underline underline-offset-[3px]">{s.title}</button>}
    </p>
  );
}

function Why({ children }: { children: ReactNode }) {
  return <p className="border-l-2 border-dotted border-muted-foreground/70 pl-2.5 text-[12px] leading-snug text-secondary-foreground">{children}</p>;
}

function Seg({ value, options, onChange, label }: { value: string; options: { value: string; label: string }[]; onChange: (v: string) => void; label: string }) {
  return <Segmented label={label} value={value} onChange={onChange} options={options} className="self-start" />;
}

/** 262144 → "256K", 1048576 → "1M". */
function tokensLabel(v: string): string {
  const n = Number(v);
  if (!Number.isFinite(n) || n < 1024) return v;
  return n % 1048576 === 0 ? `${n / 1048576}M` : n % 1024 === 0 ? `${n / 1024}K` : v;
}

function InputControl({ spec, value, onChange }: { spec: RecipeInput; value: string; onChange: (v: string) => void }) {
  if (spec.type === "choice" && spec.options?.length) {
    const opts = spec.options.map((o) => ({ value: o, label: /context|len/i.test(spec.name) ? tokensLabel(o) : o }));
    return opts.length <= 6 ? <Seg label={spec.label || spec.name} value={value} onChange={onChange} options={opts} /> : <Select aria-label={spec.label || spec.name} value={value} onValueChange={onChange} options={opts} className="h-9 w-64 text-[13px]" />;
  }
  if (spec.type === "number") {
    return (
      <span className="flex items-center gap-2.5">
        <input aria-label={spec.label || spec.name} inputMode="numeric" value={value} onChange={(e) => onChange(e.target.value)} className={cn(input, "w-28 font-mono text-[12.5px]")} />
        {spec.unit && <span className="text-[12.5px] text-muted-foreground">{spec.unit}</span>}
        {spec.by_nodes && <span className="text-[12px] text-muted-foreground">follows Runs on: {Object.entries(spec.by_nodes).map(([n, v]) => `${v} on ${n}`).join(" · ")}</span>}
      </span>
    );
  }
  return <input aria-label={spec.label || spec.name} value={value} onChange={(e) => onChange(e.target.value)} placeholder={spec.required ? "" : "optional"} className={cn(input, "w-full", spec.type === "path" || spec.name === "model" ? "font-mono text-[12.5px]" : "")} />;
}

export interface ConfigureProps {
  card: RecipeCard;
  form: DeployForm;
  base: DeployForm;
  learnedPaths: Set<FieldPath>;
  learned: Learned;
  problems: Problem[];
  resources: ClusterResources;
  cv: ClusterValues;
  weightsGb: number | null;
  view: "form" | "yaml";
  onView: (v: "form" | "yaml") => void;
  onChange: (form: DeployForm, learnedPath?: FieldPath) => void;
  onBack: () => void;
  onNext: () => void;
}

export function Configure(props: ConfigureProps) {
  const { card, form, base, learned, problems, resources, cv, view } = props;
  const p = card.preview!;
  const saved = card.source === "db";
  const changed = useMemo(() => new Set(changedPaths(form, base)), [form, base]);
  const problemAt = (path: FieldPath) => problems.find((x) => x.path === path)?.message;
  const suggestionsAt = (path: FieldPath) => learned.suggestions.filter((s) => s.path === path && String(getField(form, path) ?? "") !== String(s.value ?? ""));

  function sourceOf(path: FieldPath): Source {
    if (props.learnedPaths.has(path)) return "learned";
    if (changed.has(path)) return "changed";
    if (path.startsWith("inputs.")) {
      const spec = p.inputs.find((i) => `inputs.${i.name}` === path);
      return spec && inputSource(spec) === "cluster" ? "cluster" : "recipe";
    }
    if (path === "slurm.log_output_dir" && !p.logOutputDir && cv.logs) return "cluster";
    const v = getField(base, path);
    return v === "" || v == null ? "default" : "recipe";
  }

  function set(path: FieldPath, value: unknown) {
    let next = setField(form, path, value);
    // Inputs that follow the node count move with it, unless someone set them.
    if (path === "slurm.nodes") {
      for (const i of p.inputs) {
        if (!i.by_nodes || form.inputs[i.name] !== inputDefault(i, cv, form.slurm.nodes)) continue;
        next = setField(next, `inputs.${i.name}`, inputDefault(i, cv, Number(value)));
      }
    }
    props.onChange(next);
  }

  function apply(s: Suggestion) {
    if (s.value == null) return;
    const v = s.path === "slurm.nodes" ? Number(s.value) : String(s.value);
    let next = setField(form, s.path, v);
    if (s.path === "slurm.nodes") for (const i of p.inputs) if (i.by_nodes && form.inputs[i.name] === inputDefault(i, cv, form.slurm.nodes)) next = setField(next, `inputs.${i.name}`, inputDefault(i, cv, Number(v)));
    props.onChange(next, s.path);
  }

  const need = { gpus: gresCount(form.slurm.gres), cpus: form.slurm.cpus_per_task ? Number(form.slurm.cpus_per_task) : null, memMb: memToMb(form.slurm.mem) };
  const fits = useMemo(() => partitionFits(resources, need), [resources, need.gpus, need.cpus, need.memMb]); // eslint-disable-line react-hooks/exhaustive-deps
  const fit = fits.find((f) => f.name === form.slurm.partition);
  const flags = p.inputs.filter((i) => i.type === "flag");
  const settings = p.inputs.filter((i) => i.type !== "flag" && i.type !== "path");
  const paths = p.inputs.filter((i) => i.type === "path");
  const nodeOptions = p.nodeOptions && p.nodeOptions.length > 1 ? p.nodeOptions : null;
  const blocking = problems.length > 0;

  return (
    <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_360px]">
      <div className="flex min-w-0 flex-col gap-3.5">
        <div className="flex flex-wrap items-center justify-between gap-3 text-[12px] text-muted-foreground">
          <span className="flex flex-wrap gap-x-4 gap-y-1"><span>Where each value comes from:</span><span>{saved ? "Saved" : "Recipe"}</span><span>Cluster default</span><span className="text-foreground underline decoration-dotted underline-offset-[3px]">Learned</span><span className="font-medium text-foreground">Changed</span></span>
          <Segmented label="Edit as" value={view} onChange={props.onView} options={[{ value: "form", label: "Form" }, { value: "yaml", label: "YAML" }]} />
        </div>

        {view === "yaml" ? (
          <YamlEditor form={form} base={base} inputNames={p.inputs.map((i) => i.name)} problems={problems} learned={learned} learnedPaths={props.learnedPaths} onChange={(f) => props.onChange(f)} />
        ) : (
          <>
            <Section title="Partition" note={resources.partitions.length ? "Read from the cluster" : "The cluster's partitions couldn't be read"}>
              {resources.partitions.length ? (
                <div className="grid gap-2.5 px-5 pb-4 lg:grid-cols-3">
                  {fits.map((f) => <PartitionCard key={f.name} f={f} on={form.slurm.partition === f.name} nodes={form.slurm.nodes} onPick={() => set("slurm.partition", f.name)} />)}
                </div>
              ) : (
                <div className="px-5 pb-4"><input aria-label="Partition" value={form.slurm.partition} onChange={(e) => set("slurm.partition", e.target.value)} placeholder="Partition name" className={cn(input, "w-64 font-mono text-[12.5px]")} /></div>
              )}
              {problemAt("slurm.partition") && <p className="px-5 pb-3 text-[12px] font-medium">! {problemAt("slurm.partition")}</p>}
            </Section>

            {(nodeOptions || settings.length > 0 || flags.length > 0) && (
              <Section title="Model settings" note={engineName(p.engine) + (p.requires ? ` · ${p.requires}` : "")}>
                {nodeOptions && (
                  <Row label="Runs on" hint="More nodes split the model across them; the job starts Ray itself." source={sourceOf("slurm.nodes")} saved={saved} problem={problemAt("slurm.nodes")}>
                    <Seg label="Runs on" value={String(form.slurm.nodes)} onChange={(v) => set("slurm.nodes", Number(v))} options={nodeOptions.map((n) => ({ value: String(n), label: `${n} node${n === 1 ? "" : "s"}` }))} />
                    {suggestionsAt("slurm.nodes").map((s) => <Hint key={s.title} s={s} onApply={apply} />)}
                  </Row>
                )}
                {settings.map((i) => (
                  <Row key={i.name} label={i.label || i.name} hint={i.help} source={sourceOf(`inputs.${i.name}`)} saved={saved} problem={problemAt(`inputs.${i.name}`)}>
                    <InputControl spec={i} value={form.inputs[i.name] ?? ""} onChange={(v) => set(`inputs.${i.name}`, v)} />
                  </Row>
                ))}
                {flags.length > 0 && (
                  <Row label="Features" hint="Each adds its flags to the command." source={flags.some((f) => changed.has(`inputs.${f.name}`)) ? "changed" : "recipe"} saved={saved}>
                    <div className="flex flex-wrap gap-x-5 gap-y-2 pt-1.5 text-[13px]">
                      {flags.map((f) => (
                        <label key={f.name} className="flex items-center gap-2" title={f.adds}>
                          <input type="checkbox" checked={form.inputs[f.name] === "true"} onChange={(e) => set(`inputs.${f.name}`, e.target.checked ? "true" : "false")} className="h-4 w-4 accent-foreground" />
                          {f.label || f.name}
                        </label>
                      ))}
                    </div>
                  </Row>
                )}
              </Section>
            )}

            <Section title="Slurm job" note={resources.accounts.length || resources.qos.length ? "Accounts, QoS and nodes come from slurmrestd" : undefined}>
              <Row label="Account" hint="Charged for the job's time." source={sourceOf("slurm.account")} saved={saved} problem={problemAt("slurm.account")}>
                {resources.accounts.length ? (
                  <Select aria-label="Account" value={form.slurm.account} onValueChange={(v) => set("slurm.account", v)} className="h-9 w-72 text-[13px]" options={[{ value: "", label: "Your default account" }, ...[...new Set([form.slurm.account, ...resources.accounts])].filter(Boolean).map((a) => ({ value: a, label: a }))]} />
                ) : (
                  <input aria-label="Account" value={form.slurm.account} onChange={(e) => set("slurm.account", e.target.value)} placeholder="Your default account" className={cn(input, "w-72 font-mono text-[12.5px]")} />
                )}
                {learned.prefillWhy["slurm.account"] && props.learnedPaths.has("slurm.account") && <Why>{learned.prefillWhy["slurm.account"]}</Why>}
                {suggestionsAt("slurm.account").map((s) => <Hint key={s.title} s={s} onApply={apply} />)}
              </Row>
              <Row label="QoS" hint="Sets the longest walltime and how many jobs can queue." source={sourceOf("slurm.qos")} saved={saved} problem={problemAt("slurm.qos")}>
                {resources.qos.length ? (
                  <Seg label="QoS" value={form.slurm.qos} onChange={(v) => set("slurm.qos", v)} options={[{ value: "", label: "Default" }, ...[...new Set([form.slurm.qos, ...resources.qos])].filter(Boolean).map((q) => ({ value: q, label: q }))]} />
                ) : (
                  <input aria-label="QoS" value={form.slurm.qos} onChange={(e) => set("slurm.qos", e.target.value)} placeholder="The default QoS" className={cn(input, "w-72 font-mono text-[12.5px]")} />
                )}
                {learned.prefillWhy["slurm.qos"] && props.learnedPaths.has("slurm.qos") && <Why>{learned.prefillWhy["slurm.qos"]}</Why>}
                {suggestionsAt("slurm.qos").map((s) => <Hint key={s.title} s={s} onApply={apply} />)}
              </Row>
              <Row label="Walltime" hint="When a job's time is up, obleth submits another to keep the count." source={sourceOf("slurm.time_limit")} saved={saved} problem={problemAt("slurm.time_limit")}>
                <Walltime value={form.slurm.time_limit} max={fit?.maxMinutes ?? null} onChange={(v) => set("slurm.time_limit", v)} />
                {suggestionsAt("slurm.time_limit").map((s) => <Hint key={s.title} s={s} onApply={apply} />)}
              </Row>
              {!nodeOptions && (
                <Row label="Nodes" hint="Per replica. More than one only if the script spreads across them." source={sourceOf("slurm.nodes")} saved={saved} problem={problemAt("slurm.nodes")}>
                  <input aria-label="Nodes" inputMode="numeric" value={String(form.slurm.nodes)} onChange={(e) => set("slurm.nodes", Number(e.target.value) || 0)} className={cn(input, "w-24 font-mono text-[12.5px]")} />
                  {suggestionsAt("slurm.nodes").map((s) => <Hint key={s.title} s={s} onApply={apply} />)}
                </Row>
              )}
              <Row label="Per node" hint="From the recipe; change it only for shared nodes." source={["slurm.gres", "slurm.cpus_per_task", "slurm.mem"].some((x) => changed.has(x)) ? "changed" : sourceOf("slurm.gres")} saved={saved} problem={problemAt("slurm.cpus_per_task") ?? problemAt("slurm.mem")}>
                <div className="grid gap-2.5 sm:grid-cols-3">
                  {([["slurm.gres", "GPUs (gres)", "gpu:1"], ["slurm.cpus_per_task", "CPUs per task", "Slurm default"], ["slurm.mem", "Memory", "e.g. 560G, 0 for all"]] as const).map(([path, label, ph]) => (
                    <label key={path} className="flex flex-col gap-1 text-[12px] text-muted-foreground">{label}<input aria-label={label} value={String(getField(form, path) ?? "")} onChange={(e) => set(path, e.target.value)} placeholder={ph} className={cn(input, "font-mono text-[12.5px] text-foreground")} /></label>
                  ))}
                </div>
              </Row>
              <Row label="Only on nodes with" hint="Slurm features (constraint)." source={sourceOf("slurm.constraints")} saved={saved}>
                <Constraints value={form.slurm.constraints} features={featuresOf(resources, form.slurm.partition)} onChange={(v) => set("slurm.constraints", v)} />
              </Row>
              <Row label="Avoid nodes" hint="None of the replicas will run on these." source={sourceOf("slurm.exclude")} saved={saved}>
                <Exclude value={form.slurm.exclude} nodes={fit?.nodes.map((n) => n.name) ?? []} onChange={(v) => set("slurm.exclude", v)} />
                {suggestionsAt("slurm.exclude").map((s) => <Hint key={s.title} s={s} onApply={apply} />)}
              </Row>
            </Section>

            <Section title="Container and paths" note={<>Cluster defaults are set on the <Link href="/deployments?slurm=1" className="underline underline-offset-[3px]">Slurm connection</Link>; changes here are for this deployment only</>}>
              {paths.map((i) => (
                <Row key={i.name} label={i.label || i.name} hint={i.help} source={sourceOf(`inputs.${i.name}`)} saved={saved} problem={problemAt(`inputs.${i.name}`)}>
                  <InputControl spec={i} value={form.inputs[i.name] ?? ""} onChange={(v) => set(`inputs.${i.name}`, v)} />
                </Row>
              ))}
              <Row label="Job logs" hint="Where Slurm writes each job's output." source={sourceOf("slurm.log_output_dir")} saved={saved}>
                <input aria-label="Job logs" value={form.slurm.log_output_dir} onChange={(e) => set("slurm.log_output_dir", e.target.value)} placeholder="Slurm's default (the job's working directory)" className={cn(input, "w-full font-mono text-[12.5px]")} />
              </Row>
              {cv.setup.trim() && p.rawBody.includes("cluster.setup") && (
                <Row label="Load before launch" hint="Runs at the top of the job." source="cluster" saved={saved}>
                  <pre className="whitespace-pre-wrap rounded-lg border border-border bg-background px-3 py-2 font-mono text-[12px] text-secondary-foreground">{cv.setup}</pre>
                </Row>
              )}
              <Row label="Environment" hint="Exported at the top of the job." source={Object.keys(form.env).some((k) => changed.has(`env.${k}`)) || Object.keys(base.env).some((k) => !(k in form.env)) ? "changed" : Object.keys(form.env).length ? "recipe" : "default"} saved={saved}>
                <EnvRows env={form.env} onChange={(env) => props.onChange({ ...form, env })} />
              </Row>
            </Section>

            <Section title="Serving" note="How obleth keeps it up and routes to it">
              <Row label="API model name" hint="What clients send as model." source={sourceOf("name")} saved={saved} problem={problemAt("name")}>
                <input aria-label="API model name" value={form.name} onChange={(e) => set("name", e.target.value)} className={cn(input, "w-80 font-mono text-[12.5px]")} />
              </Row>
              <Row label="Replicas" source={["serving.keep_running", "serving.serve_from", "serving.stop_after_failed_launches"].some((x) => changed.has(x)) ? "changed" : "recipe"} saved={saved} problem={problemAt("serving.keep_running") ?? problemAt("serving.serve_from")}>
                <div className="grid gap-2.5 sm:grid-cols-3">
                  {([["serving.keep_running", "Keep running"], ["serving.serve_from", "Serve once this many are healthy"], ["serving.stop_after_failed_launches", "Stop after failed launches"]] as const).map(([path, label]) => (
                    <label key={path} className="flex flex-col gap-1 text-[12px] text-muted-foreground">{label}<input aria-label={label} inputMode="numeric" value={String(getField(form, path) ?? "")} onChange={(e) => set(path, Number(e.target.value) || 0)} className={cn(input, "font-mono text-[12.5px] text-foreground")} /></label>
                  ))}
                </div>
              </Row>
              <Row label="Health check" hint="The port comes from $OBLETH_SERVING_PORT." source={sourceOf("serving.health_path")} saved={saved} problem={problemAt("serving.health_path")}>
                <input aria-label="Health path" value={form.serving.health_path} onChange={(e) => set("serving.health_path", e.target.value)} className={cn(input, "w-56 font-mono text-[12.5px]")} />
              </Row>
            </Section>
          </>
        )}
      </div>

      <aside aria-label="Summary" className="flex flex-col gap-3.5">
        <MemoryCard weightsGb={props.weightsGb} fit={fit} resources={resources} nodes={form.slurm.nodes} />
        <PastRuns learned={learned} learnedPaths={props.learnedPaths} onApply={apply} form={form} />
        <section aria-label="Checked before submit" className="flex flex-col gap-1.5 rounded-xl border border-border bg-card px-5 py-4">
          <span className="text-sm font-semibold">Checked before submit</span>
          {problems.length === 0 ? <p className="text-[12.5px] text-secondary-foreground">✓ {resources.partitions.length ? "Everything checks out against the cluster." : "Nothing wrong that can be checked; the cluster couldn't be read."}</p> : problems.map((x) => <p key={x.path + x.message} className="text-[12.5px] font-medium">! {x.message}</p>)}
        </section>
        <div className="flex gap-2"><Button type="button" variant="outline" className="h-10" onClick={props.onBack}>Back</Button><Button type="button" className="h-10 flex-1" disabled={blocking} onClick={props.onNext}>Review the script →</Button></div>
      </aside>
    </div>
  );
}

function PartitionCard({ f, on, nodes, onPick }: { f: PartitionFit; on: boolean; nodes: number; onPick: () => void }) {
  const idle = f.idleFitting;
  return (
    <button type="button" aria-pressed={on} onClick={onPick} className={cn("flex flex-col gap-1 rounded-lg px-3.5 py-3 text-left", on ? "border-[1.5px] border-foreground bg-secondary/60" : "border border-border hover:border-muted-foreground/60", !f.fits && "opacity-60")}>
      <span className="flex justify-between gap-2"><b className="text-[13.5px] font-semibold">{f.name}</b><span className="text-[11.5px]">{f.fits ? (f.nodes.filter((n) => n.fits).length >= nodes ? "Fits" : `Has ${f.nodes.filter((n) => n.fits).length} of ${nodes} nodes`) : "Won't fit"}</span></span>
      <span className="text-xs text-muted-foreground">{f.shape || "no node details"}</span>
      <span className="text-xs text-secondary-foreground">{f.fits ? `${f.nodes.length} nodes${idle != null ? ` · ${idle} idle` : ""}${f.maxMinutes ? ` · max ${walltimeLabel(f.maxMinutes)}` : ""}` : f.reason}</span>
    </button>
  );
}

function Walltime({ value, max, onChange }: { value: string; max: number | null; onChange: (v: string) => void }) {
  const minutes = parseTimeLimit(value);
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      {walltimeChoices(max).map((m, i, all) => (
        <button key={m} type="button" aria-pressed={minutes === m} onClick={() => onChange(formatTimeLimit(m))} className={cn("inline-flex h-8 items-center rounded-full border px-3 text-[12.5px]", minutes === m ? "border-foreground bg-secondary text-foreground" : "border-border text-muted-foreground hover:text-foreground")}>
          {walltimeLabel(m)}{max && i === all.length - 1 ? " · most" : ""}
        </button>
      ))}
      <input aria-label="Walltime" value={value} onChange={(e) => onChange(e.target.value)} placeholder="D-HH:MM:SS" className={cn(input, "ml-1 w-36 font-mono text-[12.5px]")} />
    </div>
  );
}

function featuresOf(resources: ClusterResources, partition: string): string[] {
  return [...new Set(resources.nodes.filter((n) => n.partitions.includes(partition)).flatMap((n) => n.features ?? []))].sort();
}

function Constraints({ value, features, onChange }: { value: string; features: string[]; onChange: (v: string) => void }) {
  const on = new Set(value.split("&").map((x) => x.trim()).filter(Boolean));
  if (!features.length) return <input aria-label="Constraint" value={value} onChange={(e) => onChange(e.target.value)} placeholder="Any node" className={cn(input, "w-72 font-mono text-[12.5px]")} />;
  const toggle = (f: string) => {
    const next = new Set(on);
    if (next.has(f)) next.delete(f);
    else next.add(f);
    onChange([...next].join("&"));
  };
  return (
    <div className="flex flex-wrap gap-1.5 pt-0.5">
      <button type="button" aria-pressed={on.size === 0} onClick={() => onChange("")} className={cn("inline-flex h-7 items-center rounded-full border px-3 text-[12px]", on.size === 0 ? "border-foreground bg-foreground text-background" : "border-border text-muted-foreground")}>Any</button>
      {features.map((f) => <button key={f} type="button" aria-pressed={on.has(f)} onClick={() => toggle(f)} className={cn("inline-flex h-7 items-center rounded-full border px-3 font-mono text-[12px]", on.has(f) ? "border-foreground bg-secondary text-foreground" : "border-border text-muted-foreground hover:text-foreground")}>{f}</button>)}
    </div>
  );
}

function Exclude({ value, nodes, onChange }: { value: string; nodes: string[]; onChange: (v: string) => void }) {
  const list = value.split(",").map((x) => x.trim()).filter(Boolean);
  const [adding, setAdding] = useState("");
  const add = (n: string) => {
    const v = n.trim();
    if (v && !list.includes(v)) onChange([...list, v].join(","));
    setAdding("");
  };
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      {list.map((n) => (
        <span key={n} className="inline-flex h-7 items-center gap-1.5 rounded-full border border-border pl-3 pr-1.5 font-mono text-[12px]">
          {n}<button type="button" aria-label={`Stop avoiding ${n}`} onClick={() => onChange(list.filter((x) => x !== n).join(","))} className="text-muted-foreground hover:text-foreground"><X className="h-3.5 w-3.5" /></button>
        </span>
      ))}
      {nodes.length ? (
        <Select aria-label="Add a node to avoid" value="" onValueChange={add} placeholder="Add a node" searchable className="h-8 w-40 text-[12.5px]" options={nodes.filter((n) => !list.includes(n)).map((n) => ({ value: n, label: n }))} />
      ) : (
        <form onSubmit={(e) => { e.preventDefault(); add(adding); }} className="flex"><input aria-label="Node to avoid" value={adding} onChange={(e) => setAdding(e.target.value)} placeholder={list.length ? "Add another" : "None"} className={cn(input, "h-8 w-40 font-mono text-[12.5px]")} /></form>
      )}
    </div>
  );
}

function EnvRows({ env, onChange }: { env: Record<string, string>; onChange: (env: Record<string, string>) => void }) {
  // Edited as rows so a half-typed name doesn't rename the variable under the caret.
  const [rows, setRows] = useState<[string, string][]>(() => Object.entries(env));
  useEffect(() => setRows((r) => (JSON.stringify(Object.fromEntries(r.filter(([k]) => k))) === JSON.stringify(env) ? r : Object.entries(env))), [env]);
  const push = (r: [string, string][]) => {
    setRows(r);
    onChange(Object.fromEntries(r.filter(([k]) => k.trim())));
  };
  return (
    <div className="flex flex-col gap-1.5">
      {rows.map(([k, v], i) => (
        <div key={i} className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_24px] items-center gap-2">
          <input aria-label="Variable name" value={k} onChange={(e) => push(rows.map((r, j) => (j === i ? [e.target.value, r[1]] : r)))} placeholder="NAME" className={cn(input, "font-mono text-[12.5px]")} />
          <input aria-label={`Value of ${k || "variable"}`} value={v} onChange={(e) => push(rows.map((r, j) => (j === i ? [r[0], e.target.value] : r)))} placeholder="value" className={cn(input, "font-mono text-[12.5px]")} />
          <button type="button" aria-label={`Remove ${k || "variable"}`} onClick={() => push(rows.filter((_, j) => j !== i))} className="text-muted-foreground hover:text-foreground"><X className="h-4 w-4" /></button>
        </div>
      ))}
      <button type="button" onClick={() => setRows([...rows, ["", ""]])} className="self-start text-[12.5px] text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">Add a variable</button>
      <p className="text-[11.5px] text-muted-foreground">A Hugging Face token set on the Slurm connection is passed to every job as HF_TOKEN; don&apos;t put it here.</p>
    </div>
  );
}

function MemoryCard({ weightsGb, fit, resources, nodes }: { weightsGb: number | null; fit: PartitionFit | undefined; resources: ClusterResources; nodes: number }) {
  if (!weightsGb) return null;
  const members = fit ? resources.nodes.filter((n) => n.partitions.includes(fit.name)) : [];
  const nodeMb = Math.max(0, ...members.map((n) => n.real_memory_mb ?? 0));
  const totalGb = (nodeMb / 1024) * nodes;
  const share = totalGb ? Math.min(1, weightsGb / totalGb) : 0;
  return (
    <section aria-label="Memory" className="flex flex-col gap-2.5 rounded-xl border border-border bg-card px-5 py-4">
      <span className="text-sm font-semibold">Will the weights fit?</span>
      <div className="flex justify-between text-[13px]"><span>Weights</span><span className="font-mono">{weightsGb} GB</span></div>
      {totalGb > 0 ? (
        <>
          <div className="h-2 overflow-hidden rounded bg-muted"><div className={cn("h-full", share > 0.9 ? "bg-foreground" : "bg-muted-foreground")} style={{ width: `${Math.round(share * 100)}%` }} /></div>
          <p className="text-[12.5px] leading-relaxed text-secondary-foreground">
            {nodes} {fit?.name} node{nodes === 1 ? "" : "s"}: {mbLabel(nodeMb)} of memory each, as Slurm reports it, plus GPU memory. {share > 0.9 ? "That's tight once the cache and activations are added." : "The weights fit with room for the cache."}
          </p>
        </>
      ) : (
        <p className="text-[12.5px] text-muted-foreground">Pick a partition to compare with its nodes&apos; memory.</p>
      )}
      <p className="text-[11.5px] text-muted-foreground">The first start downloads the weights, which can take a while.</p>
    </section>
  );
}

function PastRuns({ learned, learnedPaths, onApply, form }: { learned: Learned; learnedPaths: Set<FieldPath>; onApply: (s: Suggestion) => void; form: DeployForm }) {
  const open = learned.suggestions.filter((s) => String(getField(form, s.path) ?? "") !== String(s.value ?? ""));
  const done = [...learnedPaths];
  if (!learned.runs && !learned.facts.length) {
    return (
      <section aria-label="From past runs" className="flex flex-col gap-1.5 rounded-xl border border-dashed border-border px-5 py-4">
        <span className="text-sm font-semibold">From past runs</span>
        <p className="text-[12.5px] leading-relaxed text-muted-foreground">Nothing yet. After a few launches, obleth suggests settings here from how those jobs went: queue waits, time to healthy, walltime cut-offs, nodes that fail.</p>
      </section>
    );
  }
  return (
    <section aria-label="From past runs" className="flex flex-col gap-3 rounded-xl border border-border bg-card px-5 py-4">
      <div className="flex items-baseline justify-between"><span className="text-sm font-semibold">From past runs</span>{learned.runs > 0 && <span className="text-[12px] text-muted-foreground">{learned.runs} launch{learned.runs === 1 ? "" : "es"} · {learned.served} served</span>}</div>
      {(open.length > 0 || done.length > 0) && (
        <div className="flex flex-col gap-2 text-[12.5px]">
          {done.map((path) => <div key={path} className="flex justify-between gap-3"><span className="text-secondary-foreground">{learned.prefillWhy[path] ?? learned.suggestions.find((s) => s.path === path)?.title ?? path}</span><span className="text-muted-foreground">Applied</span></div>)}
          {open.map((s) => (
            <div key={s.path + s.title} className="flex items-start justify-between gap-3">
              <span className="leading-snug"><span className="font-medium">{s.title}</span><span className="block text-muted-foreground">{s.evidence}</span></span>
              {s.value != null && <Button type="button" variant="outline" size="sm" className="h-7 shrink-0" onClick={() => onApply(s)}>Use</Button>}
            </div>
          ))}
        </div>
      )}
      {learned.facts.length > 0 && (
        <dl className="grid grid-cols-[minmax(0,1fr)_auto] gap-x-3 gap-y-1.5 border-t border-border pt-3 text-[12.5px]">
          {learned.facts.map((f) => <div key={f.label} className="contents"><dt className="text-muted-foreground">{f.label}</dt><dd className="text-right font-mono text-[12px]">{f.value}</dd></div>)}
        </dl>
      )}
      <p className="text-[11.5px] text-muted-foreground">From obleth&apos;s own launches. Nothing is applied until you pick it.</p>
    </section>
  );
}

function YamlEditor({ form, base, inputNames, problems, learned, learnedPaths, onChange }: { form: DeployForm; base: DeployForm; inputNames: string[]; problems: Problem[]; learned: Learned; learnedPaths: Set<FieldPath>; onChange: (f: DeployForm) => void }) {
  const [onlyChanges, setOnlyChanges] = useState(true);
  const notes = useMemo(() => {
    const n: Record<FieldPath, string> = {};
    for (const path of learnedPaths) n[path] = "learned";
    for (const s of learned.suggestions) if (!(s.path in n)) n[s.path] = `past runs: ${s.title.toLowerCase()}`;
    return n;
  }, [learned, learnedPaths]);
  const render = (only: boolean) => formToYaml(form, base, { onlyChanges: only, notes });
  const [text, setText] = useState(() => render(true));
  const [parseProblems, setParseProblems] = useState<{ line: number; message: string }[]>([]);
  const [lines, setLines] = useState<Record<FieldPath, number>>(() => yamlToForm(text, base, inputNames).lines);

  // A change made elsewhere (a suggestion applied from the side panel) rewrites
  // the text; a change typed here doesn't, so the caret stays put.
  const emitted = useRef<DeployForm>(form);
  useEffect(() => {
    if (form === emitted.current) return;
    emitted.current = form;
    const t = formToYaml(form, base, { onlyChanges, notes });
    setText(t);
    setParseProblems([]);
    setLines(yamlToForm(t, base, inputNames).lines);
  }, [form]); // eslint-disable-line react-hooks/exhaustive-deps

  function edit(t: string) {
    setText(t);
    const res = yamlToForm(t, base, inputNames);
    setParseProblems(res.problems);
    setLines(res.lines);
    if (res.form) {
      emitted.current = res.form;
      onChange(res.form);
    }
  }

  function mode(only: boolean) {
    setOnlyChanges(only);
    const t = render(only);
    setText(t);
    setParseProblems([]);
    setLines(yamlToForm(t, base, inputNames).lines);
  }

  const all = [...parseProblems, ...problems.map((x) => ({ line: lines[x.path] ?? lines[x.path.split(".")[0]] ?? 0, message: x.message }))].sort((a, b) => a.line - b.line);
  const count = text.split("\n").length;
  const download = () => {
    const url = URL.createObjectURL(new Blob([text], { type: "application/yaml" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = `${form.name || "deployment"}.deploy.yaml`;
    a.click();
    URL.revokeObjectURL(url);
  };

  return (
    <section aria-label="Deployment YAML" className="overflow-hidden rounded-xl border border-border bg-card">
      <div className="flex flex-wrap items-center justify-between gap-2 border-b border-border px-4 py-2.5">
        <span className="font-mono text-[12.5px]">{form.name || "deployment"}.deploy.yaml</span>
        <span className="flex items-center gap-2">
          <Segmented label="Show" value={onlyChanges ? "changes" : "all"} onChange={(v) => mode(v === "changes")} options={[{ value: "changes", label: "Only what's set here" }, { value: "all", label: "Everything" }]} />
          <Button type="button" variant="outline" size="sm" className="h-8" onClick={() => navigator.clipboard?.writeText(text)}>Copy</Button>
          <Button type="button" variant="outline" size="sm" className="h-8" onClick={download}>Download</Button>
        </span>
      </div>
      <div className="grid grid-cols-[44px_minmax(0,1fr)] font-mono text-[12.5px] leading-[21px]">
        <div aria-hidden className="select-none py-3 pr-3 text-right text-muted-foreground/60">{Array.from({ length: count }, (_, i) => <div key={i} className={cn(all.some((x) => x.line === i + 1) && "font-semibold text-foreground")}>{i + 1}</div>)}</div>
        <textarea aria-label="Deployment YAML" value={text} onChange={(e) => edit(e.target.value)} spellCheck={false} rows={Math.max(count + 1, 14)} className="w-full resize-y bg-transparent py-3 pr-4 leading-[21px] outline-none" />
      </div>
      <div className="flex flex-col gap-1 border-t border-border px-4 py-2.5 text-[12.5px]">
        {all.length === 0 ? <span className="text-secondary-foreground">✓ Same deployment as the form. Keys are checked against the recipe&apos;s inputs and what slurmrestd reports.</span> : all.map((x, i) => <span key={i}><span className="font-mono font-medium">{x.line ? `Line ${x.line}` : "—"}</span> {x.message}</span>)}
      </div>
    </section>
  );
}
