"use client";

import { useMemo, useState, useTransition } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Check, Search } from "lucide-react";
import { launchRecipeAction } from "@/app/actions";
import { recipeAsks } from "@/components/deployments/recipes-tab";
import { Field, TextField } from "@/components/models/fields";
import { Notice } from "@/components/models/ui";
import { Pill, Segmented } from "@/components/overview/ui";
import type { RecipeCard } from "@/components/recipes/recipe-card";
import { TemplateEditor } from "@/components/recipes/template-editor";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";
import {
  deploymentHref,
  formatTimeLimit,
  gresCount,
  memToMb,
  parseTimeLimit,
  partitionFits,
  preflightChecks,
  walltimeChoices,
  walltimeLabel,
  type PartitionFit,
} from "@/lib/deployments-model";
import { MODEL_TYPE_NAMES } from "@/lib/models-model";
import { useClusterResources } from "@/lib/use-cluster-resources";
import { cn } from "@/lib/utils";

type Step = 1 | 2 | 3;

function Steps({ step, onPick, reached }: { step: Step; onPick: (s: Step) => void; reached: Step }) {
  const items: { n: Step; label: string }[] = [{ n: 1, label: "Recipe" }, { n: 2, label: "Where it runs" }, { n: 3, label: "Review" }];
  return (
    <ol className="flex flex-wrap items-center gap-2.5 text-[13px]" aria-label="Steps">
      {items.map((it, i) => (
        <li key={it.n} className="flex items-center gap-2.5">
          {i > 0 && <span className="h-px w-8 bg-border" aria-hidden />}
          <button type="button" disabled={it.n > reached} onClick={() => onPick(it.n)} aria-current={step === it.n ? "step" : undefined} className={cn("inline-flex items-center gap-2 disabled:cursor-default", step === it.n ? "font-semibold text-foreground" : "text-muted-foreground")}>
            <span className={cn("inline-flex h-[22px] w-[22px] items-center justify-center rounded-full border text-[11.5px] font-semibold", step === it.n ? "border-foreground bg-foreground text-background" : it.n < step ? "border-muted-foreground/60 bg-muted-foreground/40 text-foreground" : "border-border")}>
              {it.n < step ? <Check className="h-3 w-3" /> : it.n}
            </span>
            {it.label}
          </button>
        </li>
      ))}
    </ol>
  );
}

function fitFor(fits: PartitionFit[], partition: string | undefined): PartitionFit | undefined {
  return fits.find((f) => f.name === partition);
}

/** A recipe's script with its {{variables}} filled, for the review. */
function fill(body: string, values: Record<string, string>): string {
  return body.replace(/\{\{([a-zA-Z_][a-zA-Z0-9_]*)\}\}/g, (m, name) => (values[name]?.trim() ? values[name] : m));
}

export function NewDeployment({ recipes, takenNames, slurmOn, initialRecipe }: { recipes: RecipeCard[]; takenNames: string[]; slurmOn: boolean; initialRecipe?: string }) {
  const router = useRouter();
  const resources = useClusterResources();
  const usable = recipes.filter((r) => r.valid && r.preview);
  const [step, setStep] = useState<Step>(1);
  const [reached, setReached] = useState<Step>(1);
  const [query, setQuery] = useState("");
  const [engine, setEngine] = useState("all");
  const [picked, setPicked] = useState<string>(usable.find((r) => r.id === initialRecipe)?.id ?? usable[0]?.id ?? "");
  const recipe = usable.find((r) => r.id === picked);
  const p = recipe?.preview;
  const [editor, setEditor] = useState(false);
  const [pending, start] = useTransition();
  const [error, setError] = useState<string | null>(null);

  // Everything below starts from the recipe and resets when another is picked.
  const [vars, setVars] = useState<Record<string, string>>({});
  const [partition, setPartition] = useState("");
  const [gres, setGres] = useState("");
  const [cpus, setCpus] = useState("");
  const [mem, setMem] = useState("");
  const [account, setAccount] = useState("");
  const [qos, setQos] = useState("");
  const [time, setTime] = useState("");
  const [target, setTarget] = useState("2");
  const [minReplicas, setMinReplicas] = useState("1");
  const [failures, setFailures] = useState("3");
  const [name, setName] = useState("");
  const [script, setScript] = useState<string | null>(null);

  function choose(id: string) {
    const r = usable.find((x) => x.id === id);
    const pv = r?.preview;
    setPicked(id);
    if (!pv) return;
    setVars(Object.fromEntries((pv.variables ?? []).map((v) => [v.name, v.default ?? ""])));
    setPartition(pv.partition ?? "");
    setGres(pv.gres ?? "");
    setCpus(pv.cpusPerTask ? String(pv.cpusPerTask) : "");
    setMem(pv.mem ?? "");
    setAccount(pv.account ?? "");
    setQos(pv.qos ?? "");
    setTime(pv.timeLimit ?? "");
    setTarget(String(pv.targetReplicas ?? 2));
    setMinReplicas("1");
    setFailures(String(pv.maxJobFailures ?? 3));
    setName(pv.apiModelName);
    setScript(null);
  }
  // First paint: fill from the preselected recipe once.
  const [seeded, setSeeded] = useState(false);
  if (!seeded && recipe) { setSeeded(true); choose(recipe.id); }

  const engines = [...new Set(usable.map((r) => r.engine).filter((e): e is string => !!e))].sort();
  const q = query.trim().toLowerCase();
  const shown = usable.filter((r) => (engine === "all" || r.engine === engine) && (!q || `${r.name} ${r.engine} ${r.description} ${r.apiModelName}`.toLowerCase().includes(q)));
  const need = { gpus: gresCount(gres), cpus: cpus ? Number(cpus) : null, memMb: memToMb(mem) };
  const fits = useMemo(() => partitionFits(resources, need), [resources, need.gpus, need.cpus, need.memMb]); // eslint-disable-line react-hooks/exhaustive-deps
  const fit = fitFor(fits, partition);
  const minutes = parseTimeLimit(time);
  const missing = (p?.variables ?? []).filter((v) => v.required && !vars[v.name]?.trim());
  const nameTrim = name.trim();
  const nameFree = !!nameTrim && !takenNames.includes(nameTrim);
  const unknownPartition = resources.partitions.length > 0 && !!partition && !fit;
  const checks = [
    ...(unknownPartition ? [{ ok: false, text: `There's no ${partition} partition on this cluster` }] : []),
    ...preflightChecks({ fit: resources.partitions.length ? fit : undefined, minutes, nameFree, name: nameTrim, account, accounts: resources.accounts, qos, qosList: resources.qos }),
  ];
  const blocking = checks.filter((c) => !c.ok && !(c.text === "Pick a partition" && partition));
  const cardFit = (r: RecipeCard) => {
    if (!resources.partitions.length || !r.preview) return null;
    const f = partitionFits(resources, { gpus: gresCount(r.preview.gres), cpus: r.preview.cpusPerTask ?? null, memMb: memToMb(r.preview.mem) });
    const ok = f.filter((x) => x.fits).map((x) => x.name);
    return ok.length ? { ok: true, text: `Fits ${ok.slice(0, 2).join(", ")}${ok.length > 2 ? ` +${ok.length - 2}` : ""}` } : { ok: false, text: "Won't fit any partition" };
  };

  function go(to: Step) {
    setStep(to);
    setReached((r) => (to > r ? to : r));
    window.scrollTo({ top: 0 });
  }

  function launch() {
    if (!recipe) return;
    setError(null);
    start(async () => {
      const res = await launchRecipeAction(recipe.id, {
        api_model_name: nameTrim,
        partition,
        gres,
        cpus_per_task: cpus ? Number(cpus) : null,
        mem,
        account,
        qos,
        time_limit: time,
        target_replicas: Number(target) || 1,
        min_replicas: Math.min(Number(minReplicas) || 1, Number(target) || 1),
        max_job_failures: Number(failures) || 0,
        variables: vars,
        ...(script !== null ? { script_body: script } : {}),
      });
      if (!res.ok) return setError(res.error);
      router.push(deploymentHref(res.name));
    });
  }

  const previewBody = fill(script ?? p?.rawBody ?? "", vars);
  const aside = "flex flex-col gap-4 border-t border-border bg-background/40 px-[22px] py-5 xl:border-l xl:border-t-0";

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-4">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          <p className="text-[12.5px] text-muted-foreground"><Link href="/deployments" className="text-secondary-foreground hover:text-foreground">Deployments</Link> / New{recipe && step > 1 ? ` · ${recipe.name}` : ""}</p>
          <h1 className="text-[26px] font-semibold tracking-tight">{step === 1 ? "New deployment" : step === 2 ? "Where should it run?" : "Review and launch"}</h1>
        </div>
        <Steps step={step} onPick={go} reached={reached} />
      </div>

      {!slurmOn && (
        <Notice strong>Slurm isn&apos;t set up on this gateway yet, so nothing can be launched. <Link href="/settings?tab=slurm" className="underline underline-offset-2">Set up Slurm ›</Link></Notice>
      )}

      <div className="grid min-h-[560px] overflow-hidden rounded-xl border border-border bg-card xl:grid-cols-[minmax(0,1fr)_400px]">
        {step === 1 && (
          <>
            <section className="flex min-w-0 flex-col gap-4 px-6 py-5">
              <div className="flex flex-wrap items-center gap-2">
                <label className="flex h-9 min-w-[220px] flex-1 items-center gap-2 rounded-lg border border-border bg-background px-3 text-[13px] focus-within:border-muted-foreground">
                  <Search className="h-4 w-4 shrink-0 text-muted-foreground" aria-hidden />
                  <input value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search recipes" placeholder="Recipe, engine or model" className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground" />
                </label>
                {engines.length > 1 && <Segmented label="Engine" value={engine} onChange={setEngine} options={[{ value: "all", label: `All ${usable.length}` }, ...engines.map((e) => ({ value: e, label: e }))]} />}
              </div>
              {usable.length === 0 && <p className="rounded-lg border border-dashed border-border px-4 py-6 text-center text-[13px] text-muted-foreground">No recipes yet. Write one from a script you already run on the cluster.</p>}
              <div className="grid gap-3 lg:grid-cols-2">
                {shown.map((r) => {
                  const on = r.id === picked;
                  const f = cardFit(r);
                  const toFill = (r.preview?.variables ?? []).filter((v) => v.required && !v.default).length;
                  return (
                    <button key={`${r.source}:${r.id}`} type="button" aria-pressed={on} onClick={() => choose(r.id)} className={cn("flex flex-col gap-2.5 rounded-xl px-4 py-3.5 text-left", on ? "border-[1.5px] border-foreground bg-secondary/60" : "border border-border hover:border-muted-foreground/60")}>
                      <span className="flex w-full items-start justify-between gap-2"><span className="text-[14.5px] font-semibold">{r.name}</span><Pill>{r.source === "file" ? "File" : "Saved"}</Pill></span>
                      <span className="text-[12.5px] leading-relaxed text-muted-foreground">{r.description || `Serves ${r.apiModelName}.`}</span>
                      <span className="flex flex-wrap gap-1.5 text-[11.5px]">
                        {r.engine && <span className="rounded-md bg-muted px-2 py-0.5 text-secondary-foreground">{r.engine}</span>}
                        <span className="rounded-md bg-muted px-2 py-0.5 font-mono text-secondary-foreground">{recipeAsks(r)}</span>
                        {f && <span className={cn("rounded-md px-2 py-0.5", f.ok ? "border border-muted-foreground/60 text-foreground" : "bg-muted text-muted-foreground")}>{f.text}</span>}
                        {toFill > 0 && <span className="rounded-md bg-foreground px-2 py-0.5 font-semibold text-background">{toFill} value{toFill === 1 ? "" : "s"} to fill in</span>}
                      </span>
                    </button>
                  );
                })}
              </div>
              <button type="button" onClick={() => setEditor(true)} className="flex flex-col gap-1 rounded-xl border border-dashed border-border px-4 py-3.5 text-left hover:border-muted-foreground/60">
                <span className="text-sm font-semibold">Write a recipe from an sbatch script</span>
                <span className="text-[12.5px] text-muted-foreground">Paste a script you already run; its #SBATCH lines become placement settings. It&apos;s saved so you can launch it again.</span>
              </button>
            </section>
            <aside className={aside}>
              {recipe && p ? (
                <>
                  <div className="flex flex-col gap-1"><span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Selected</span><span className="text-[17px] font-semibold">{recipe.name}</span>{recipe.description && <span className="text-[12.5px] leading-relaxed text-muted-foreground">{recipe.description}</span>}</div>
                  {(p.variables ?? []).length > 0 && (
                    <div className={cn("flex flex-col gap-3 rounded-xl px-3.5 py-3", missing.length ? "border-[1.5px] border-foreground" : "border border-border")}>
                      <span className="text-[13px] font-semibold">{(p.variables ?? []).length} value{(p.variables ?? []).length === 1 ? "" : "s"} for your cluster</span>
                      {(p.variables ?? []).map((v) => (
                        <Field key={v.name} name={`var_${v.name}`} label={`${v.label || v.name}${v.required ? "" : " (optional)"}`} value={vars[v.name] ?? ""} onChange={(e) => setVars((x) => ({ ...x, [v.name]: e.target.value }))} placeholder={v.default || v.name} className="[&_input]:font-mono" />
                      ))}
                    </div>
                  )}
                  <div className="overflow-hidden rounded-xl border border-border text-[13px]">
                    {[["API model name", p.apiModelName], ["Type", MODEL_TYPE_NAMES[p.modelType] ?? p.modelType], ["Engine", p.engine], ["Replicas", String(p.targetReplicas)], ["Script", `${p.rawBody.split("\n").length} lines`]].map(([k, v]) => (
                      <div key={k} className="flex justify-between gap-3 border-t border-border px-3.5 py-2.5 first:border-t-0"><span className="text-muted-foreground">{k}</span><span className={cn(k === "API model name" && "font-mono text-[12.5px]")}>{v}</span></div>
                    ))}
                  </div>
                  {p.warnings.length > 0 && <p className="text-xs text-muted-foreground">{p.warnings.join(" ")}</p>}
                  <div className="flex-1" />
                  <Button type="button" className="h-10" disabled={missing.length > 0} onClick={() => go(2)}>{missing.length ? `Fill in ${missing.map((m) => m.label || m.name).join(", ")}` : "Choose where it runs →"}</Button>
                </>
              ) : (
                <p className="text-[13px] text-muted-foreground">Pick a recipe.</p>
              )}
            </aside>
          </>
        )}

        {step === 2 && p && (
          <>
            <section className="flex min-w-0 flex-col gap-4 px-6 py-5">
              <div className="flex flex-col gap-2">
                <span className="text-sm font-semibold">Partition</span>
                <span className="text-xs text-muted-foreground">Each replica asks for {[gres || "no GPU", cpus ? `${cpus} CPU` : null, mem || null].filter(Boolean).join(", ")}.</span>
                {resources.partitions.length ? (
                  <div className="grid gap-2.5 lg:grid-cols-3">
                    {fits.map((f) => (
                      <button key={f.name} type="button" aria-pressed={partition === f.name} onClick={() => setPartition(f.name)} className={cn("flex flex-col gap-1 rounded-lg px-3.5 py-3 text-left", partition === f.name ? "border-[1.5px] border-foreground bg-secondary/60" : "border border-border hover:border-muted-foreground/60", !f.fits && "opacity-60")}>
                        <span className="flex justify-between gap-2"><b className="text-[13.5px] font-semibold">{f.name}</b><span className="text-[11.5px]">{f.fits ? "Fits" : "Won't fit"}</span></span>
                        <span className="text-xs text-muted-foreground">{f.shape || "no node details"}</span>
                        <span className="text-xs text-secondary-foreground">{f.fits ? `${f.nodes.length} nodes${f.idleFitting != null ? ` · ${f.idleFitting} idle` : ""}${f.maxMinutes ? ` · max ${walltimeLabel(f.maxMinutes)}` : ""}` : f.reason}</span>
                      </button>
                    ))}
                  </div>
                ) : (
                  <TextField name="partition" label="Partition" value={partition} onChange={(e) => setPartition(e.target.value)} placeholder="Partition name" mono className="w-64" />
                )}
              </div>
              <div className="grid gap-3 sm:grid-cols-3">
                <Field label="GPUs (gres)" name="gres" value={gres} onChange={(e) => setGres(e.target.value)} placeholder="gpu:1" className="[&_input]:font-mono" />
                <Field label="CPUs" name="cpus" inputMode="numeric" value={cpus} onChange={(e) => setCpus(e.target.value)} placeholder="partition default" className="[&_input]:font-mono" />
                <Field label="Memory" name="mem" value={mem} onChange={(e) => setMem(e.target.value)} placeholder="e.g. 560G" className="[&_input]:font-mono" />
              </div>
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="space-y-1.5">
                  <span className="text-[12.5px] font-medium text-secondary-foreground">Account</span>
                  {resources.accounts.length ? <Select aria-label="Account" value={account} onValueChange={setAccount} className="h-9 text-[13px]" options={[{ value: "", label: "Your default account" }, ...[...new Set([account, ...resources.accounts])].filter(Boolean).map((a) => ({ value: a, label: a }))]} /> : <TextField name="account" label="Account" value={account} onChange={(e) => setAccount(e.target.value)} placeholder="Your default account" mono />}
                </div>
                <div className="space-y-1.5">
                  <span className="text-[12.5px] font-medium text-secondary-foreground">QoS</span>
                  {resources.qos.length ? <Select aria-label="QoS" value={qos} onValueChange={setQos} className="h-9 text-[13px]" options={[{ value: "", label: "The default QoS" }, ...[...new Set([qos, ...resources.qos])].filter(Boolean).map((a) => ({ value: a, label: a }))]} /> : <TextField name="qos" label="QoS" value={qos} onChange={(e) => setQos(e.target.value)} placeholder="The default QoS" mono />}
                </div>
              </div>
              <div className="flex flex-col gap-2">
                <span className="text-[12.5px] font-medium text-secondary-foreground">Walltime per job <span className="font-normal text-muted-foreground">· when a job&apos;s time is up, obleth submits a new one</span></span>
                <div className="flex flex-wrap items-center gap-1.5">
                  {walltimeChoices(fit?.maxMinutes ?? null).map((m, i, all) => (
                    <button key={m} type="button" aria-pressed={minutes === m} onClick={() => setTime(formatTimeLimit(m))} className={cn("inline-flex h-8 items-center rounded-full border px-3 text-[12.5px]", minutes === m ? "border-foreground bg-secondary text-foreground" : "border-border text-muted-foreground hover:text-foreground")}>
                      {walltimeLabel(m)}{fit?.maxMinutes && i === all.length - 1 ? " · most" : ""}
                    </button>
                  ))}
                  <TextField name="time" label="Walltime" value={time} onChange={(e) => setTime(e.target.value)} placeholder="D-HH:MM:SS" mono className="ml-1 w-36" />
                </div>
              </div>
              <div className="flex flex-wrap items-end gap-3">
                <Field label="Keep running" name="target" inputMode="numeric" value={target} onChange={(e) => setTarget(e.target.value)} className="w-32" />
                <Field label="Serve from" name="min" inputMode="numeric" value={minReplicas} onChange={(e) => setMinReplicas(e.target.value)} className="w-32" />
                <Field label="Stop after failed launches" name="failures" inputMode="numeric" value={failures} onChange={(e) => setFailures(e.target.value)} className="w-48" />
              </div>
            </section>
            <aside className={aside}>
              {fit && fit.nodes.length > 0 && (
                <div className="flex flex-col gap-2">
                  <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">{fit.name} nodes</span>
                  <div className="flex flex-wrap gap-1">
                    {fit.nodes.slice(0, 64).map((n) => (
                      <span key={n.name} title={`${n.name}: ${n.state}${n.fits ? "" : ", too small"}`} className={cn("h-5 w-7 rounded-[4px]", n.state === "idle" ? "bg-foreground" : n.state === "busy" ? "bg-muted-foreground/50" : "border border-dashed border-muted-foreground", !n.fits && "opacity-40")} />
                    ))}
                  </div>
                  <span className="flex gap-3 text-[11.5px] text-muted-foreground"><span>■ idle</span><span>■ busy</span><span>▢ down or draining</span></span>
                  {fit.idleFitting != null && <p className="text-[13px]">{fit.idleFitting >= (Number(target) || 1) ? `${fit.idleFitting} idle node${fit.idleFitting === 1 ? "" : "s"} fit. The replicas can start straight away.` : fit.idleFitting > 0 ? `${fit.idleFitting} idle node${fit.idleFitting === 1 ? "" : "s"} fit; the rest queue until nodes free up.` : "No idle node fits right now; the jobs queue until one frees up."}</p>}
                </div>
              )}
              <div className="flex flex-col gap-1.5 border-t border-border pt-3">
                <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Checked before submit</span>
                {checks.filter((c) => !c.text.startsWith("The name") && !c.text.startsWith("A model called")).map((c) => (
                  <p key={c.text} className={cn("flex gap-2 text-[12.5px]", c.ok ? "text-secondary-foreground" : "font-medium text-foreground")}><span aria-hidden>{c.ok ? "✓" : "!"}</span>{c.text}</p>
                ))}
                {!resources.partitions.length && <p className="text-[12.5px] text-muted-foreground">The cluster&apos;s partitions couldn&apos;t be read, so these are free text and nothing is checked.</p>}
              </div>
              <div className="flex flex-col gap-1.5 border-t border-border pt-3">
                <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Sent to Slurm as</span>
                <p className="font-mono text-[11.5px] leading-relaxed text-secondary-foreground">{[`partition=${partition || "?"}`, account && `account=${account}`, qos && `qos=${qos}`, time && `time=${time}`, gres && `gres=${gres}`, cpus && `cpus=${cpus}`, mem && `mem=${mem}`].filter(Boolean).join(" ")}</p>
              </div>
              <div className="flex-1" />
              <div className="flex gap-2"><Button type="button" variant="outline" className="h-10" onClick={() => go(1)}>Back</Button><Button type="button" className="h-10 flex-1" disabled={!partition} onClick={() => go(3)}>Review →</Button></div>
            </aside>
          </>
        )}

        {step === 3 && p && (
          <>
            <section className="flex min-w-0 flex-col gap-4 px-6 py-5">
              <div className="flex flex-col gap-2">
                <span className="text-sm font-semibold">The model</span>
                <span className="text-xs text-muted-foreground">What clients send as model. It appears in Models once you launch.</span>
                <div className="flex flex-wrap items-center gap-3">
                  <TextField name="api_model_name" label="API model name" value={name} onChange={(e) => setName(e.target.value)} mono className="w-72" />
                  <span className={cn("text-xs", nameFree ? "text-secondary-foreground" : "font-medium text-foreground")}>{!nameTrim ? "Name it" : nameFree ? "✓ free" : "A model with this name already exists"}</span>
                  <span className="text-xs text-muted-foreground">{MODEL_TYPE_NAMES[p.modelType] ?? p.modelType}</span>
                </div>
              </div>
              <div className="flex flex-col gap-2">
                <div className="flex items-center justify-between gap-3">
                  <div><span className="text-sm font-semibold">Job script</span><p className="text-xs text-muted-foreground">What each replica&apos;s job runs, with your values filled in. obleth adds the port binding before it.</p></div>
                  <Button type="button" variant="outline" size="sm" onClick={() => setScript(script === null ? p.rawBody : null)}>{script === null ? "Edit" : "Undo edits"}</Button>
                </div>
                {script === null ? (
                  <pre className="max-h-[440px] overflow-auto rounded-lg border border-border bg-background px-3 py-2.5 font-mono text-[12px] leading-relaxed text-secondary-foreground">
                    <span className="text-muted-foreground"># obleth: binds the first free port in this replica&apos;s window and exports OBLETH_SERVING_PORT{"\n"}</span>
                    {previewBody}
                  </pre>
                ) : (
                  <textarea aria-label="Job script" value={script} onChange={(e) => setScript(e.target.value)} rows={18} spellCheck={false} className="w-full rounded-lg border border-input bg-background px-3 py-2.5 font-mono text-[12px] leading-relaxed focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring" />
                )}
              </div>
            </section>
            <aside className={aside}>
              <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">When you launch</span>
              <ol className="flex flex-col gap-3 text-[13px] leading-relaxed text-secondary-foreground">
                <li className="grid grid-cols-[20px_minmax(0,1fr)] gap-2"><span className="font-mono text-muted-foreground">1</span><span>Adds <span className="font-mono">{nameTrim || "…"}</span> to Models. Requests get 503 until a replica is healthy.</span></li>
                <li className="grid grid-cols-[20px_minmax(0,1fr)] gap-2"><span className="font-mono text-muted-foreground">2</span><span>Submits {target || 1} job{Number(target) === 1 ? "" : "s"} to {partition}{account ? ` as ${account}${qos ? ` / ${qos}` : ""}` : ""}{minutes ? `, ${walltimeLabel(minutes)} each` : ""}.</span></li>
                <li className="grid grid-cols-[20px_minmax(0,1fr)] gap-2"><span className="font-mono text-muted-foreground">3</span><span>Checks <span className="font-mono">:{p.port}{p.healthPath}</span> while the model loads, then warms it up with a one-token request.</span></li>
                <li className="grid grid-cols-[20px_minmax(0,1fr)] gap-2"><span className="font-mono text-muted-foreground">4</span><span>Puts each healthy replica in rotation.</span></li>
                <li className="grid grid-cols-[20px_minmax(0,1fr)] gap-2"><span className="font-mono text-muted-foreground">5</span><span>Keeps {target || 1} running: replaces a job that ends{Number(failures) > 0 ? `, and stops after ${failures} failed launches in a row` : ""}.</span></li>
              </ol>
              <div className="border-t border-border pt-3 text-[13px]">
                {blocking.length === 0 ? <span>{resources.partitions.length ? "All checks passed." : "Nothing could be checked against the cluster."}</span> : blocking.map((c) => <p key={c.text} className="font-medium">! {c.text}</p>)}
              </div>
              {error && <Notice strong>{error}</Notice>}
              <div className="flex-1" />
              <div className="flex gap-2">
                <Button type="button" variant="outline" className="h-10" onClick={() => go(2)}>Back</Button>
                <Button type="button" className="h-10 flex-1" disabled={pending || !slurmOn || !nameFree || !partition} onClick={launch}>{pending ? "Launching…" : `Launch ${target || 1} replica${Number(target) === 1 ? "" : "s"}`}</Button>
              </div>
            </aside>
          </>
        )}
      </div>

      <TemplateEditor open={editor} onOpenChange={setEditor} onSaved={() => router.refresh()} />
    </div>
  );
}
