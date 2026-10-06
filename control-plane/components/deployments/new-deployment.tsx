"use client";

import { useMemo, useState, useTransition } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Check } from "lucide-react";
import { launchRecipeAction, saveRecipeFromFormAction } from "@/app/actions";
import { Configure } from "@/components/deployments/new/configure";
import { Picker, type Picked, type PickerTab } from "@/components/deployments/new/picker";
import { Review } from "@/components/deployments/new/review";
import { Notice } from "@/components/models/ui";
import type { RecipeCard } from "@/components/recipes/recipe-card";
import { baselineForm, formProblems, getField, setField, toOverrides, type DeployForm, type FieldPath } from "@/lib/deploy-form";
import { deploymentHref } from "@/lib/deployments-model";
import { learnFrom, type LaunchRecord } from "@/lib/launch-learning";
import { EMPTY_CLUSTER, type ClusterValues } from "@/lib/recipe-inputs";
import { savedName } from "@/lib/recipe-save";
import { useClusterResources } from "@/lib/use-cluster-resources";
import { cn } from "@/lib/utils";

type Step = 1 | 2 | 3;

function Steps({ step, onPick, reached }: { step: Step; onPick: (s: Step) => void; reached: Step }) {
  const items: { n: Step; label: string }[] = [{ n: 1, label: "Model" }, { n: 2, label: "Where and how" }, { n: 3, label: "Review" }];
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

export function NewDeployment({ recipes, takenNames, slurmOn, initialRecipe, cluster = EMPTY_CLUSTER, launches = [] }: {
  recipes: RecipeCard[];
  takenNames: string[];
  slurmOn: boolean;
  initialRecipe?: string;
  cluster?: ClusterValues;
  launches?: LaunchRecord[];
}) {
  const router = useRouter();
  const resources = useClusterResources();
  const usable = recipes.filter((r) => r.valid && r.preview);
  const initial = usable.find((r) => r.id === initialRecipe);
  const hasSaved = usable.some((r) => r.source === "db");
  const [tab, setTab] = useState<PickerTab>(initial ? (initial.source === "db" ? "saved" : "library") : hasSaved ? "saved" : "library");
  const [picked, setPicked] = useState<Picked | null>(initial ? { recipeId: initial.id } : null);
  const [step, setStep] = useState<Step>(1);
  const [reached, setReached] = useState<Step>(1);
  const [base, setBase] = useState<DeployForm | null>(null);
  const [form, setForm] = useState<DeployForm | null>(null);
  const [learnedPaths, setLearnedPaths] = useState<Set<FieldPath>>(new Set());
  const [view, setView] = useState<"form" | "yaml">("form");
  const [script, setScript] = useState<string | null>(null);
  const [saveAs, setSaveAs] = useState({ on: false, name: "" });
  const [pending, start] = useTransition();
  const [error, setError] = useState<string | null>(null);

  const card = usable.find((r) => r.id === (form?.recipe ?? picked?.recipeId));
  const partitionMax = useMemo(() => {
    const p = resources.partitions.find((x) => x.name === form?.slurm.partition);
    const n = p?.max_time ? Number(p.max_time) : null;
    return n && Number.isFinite(n) && n > 0 ? n : null;
  }, [resources, form?.slurm.partition]);
  const learned = useMemo(() => (form ? learnFrom(launches, form, { partitionMaxMinutes: partitionMax }) : null), [launches, form, partitionMax]);
  const problems = useMemo(() => (form && card?.preview ? formProblems(form, card.preview, resources, takenNames) : []), [form, card, resources, takenNames]);

  function go(to: Step) {
    setStep(to);
    setReached((r) => (to > r ? to : r));
    window.scrollTo({ top: 0 });
  }

  /** Step 1 → 2: start the form from the recipe, then fill what history agrees on. */
  function begin() {
    const c = usable.find((r) => r.id === picked?.recipeId);
    if (!c?.preview || !picked) return;
    if (form && form.recipe === c.id && base && reached > 1 && !picked.hf) return go(2);
    let b = baselineForm(c.id, c.preview, cluster, { name: picked.name, inputs: picked.inputs });
    // A saved recipe whose name is already taken starts with a free variant.
    if (takenNames.includes(b.name)) {
      let i = 2;
      while (takenNames.includes(`${b.name}-${i}`)) i += 1;
      b = { ...b, name: `${b.name}-${i}` };
    }
    const l = learnFrom(launches, b);
    let f = b;
    for (const [path, value] of Object.entries(l.prefill)) f = setField(f, path, value);
    setBase(b);
    setForm(f);
    setLearnedPaths(new Set(Object.keys(l.prefill)));
    setScript(null);
    setSaveAs({ on: false, name: savedName(c.name ?? c.id, f) });
    setError(null);
    go(2);
  }

  function change(next: DeployForm, learnedPath?: FieldPath) {
    setForm(next);
    setLearnedPaths((prev) => {
      const out = new Set<FieldPath>();
      // A learned value stays "learned" only while nobody changes it.
      for (const p of prev) if (JSON.stringify(getField(next, p)) === JSON.stringify(form && getField(form, p))) out.add(p);
      if (learnedPath) out.add(learnedPath);
      return out;
    });
  }

  function launch() {
    if (!form || !card) return;
    setError(null);
    start(async () => {
      if (saveAs.on) {
        const saved = await saveRecipeFromFormAction(card.id, form, { name: saveAs.name, model: card.preview?.kind === "engine" ? form.inputs.model : undefined, weightsGb: picked?.hf?.weightsGb ?? null });
        if (!saved.ok) return setError(`Not launched: saving the recipe failed. ${saved.error}`);
      }
      const res = await launchRecipeAction(card.id, { ...toOverrides(form), ...(script !== null ? { script_body: script } : {}) });
      if (!res.ok) return setError(res.error);
      router.push(deploymentHref(res.name));
    });
  }

  const title = step === 1 ? "What do you want to run?" : step === 2 ? "Where and how it runs" : "Review and launch";
  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-4">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          <p className="text-[12.5px] text-muted-foreground"><Link href="/deployments" className="text-secondary-foreground hover:text-foreground">Deployments</Link> / New{card && step > 1 ? ` · ${card.name}` : ""}</p>
          <h1 className="text-[26px] font-semibold tracking-tight">{title}</h1>
        </div>
        <Steps step={step} onPick={(s) => (s === 2 && step === 1 ? begin() : go(s))} reached={reached} />
      </div>

      {!slurmOn && (
        <Notice strong>Slurm isn&apos;t set up on this gateway yet, so nothing can be launched. <Link href="/deployments?slurm=1" className="underline underline-offset-2">Set up Slurm ›</Link></Notice>
      )}

      {step === 1 && <Picker recipes={recipes} launches={launches} resources={resources} tab={tab} onTab={setTab} picked={picked} onPick={setPicked} onNext={begin} />}
      {step === 2 && form && base && card?.preview && learned && (
        <Configure card={card} form={form} base={base} learnedPaths={learnedPaths} learned={learned} problems={problems} resources={resources} cv={cluster} weightsGb={picked?.hf?.weightsGb ?? card.preview.weightsGb ?? null} view={view} onView={setView} onChange={change} onBack={() => go(1)} onNext={() => go(3)} />
      )}
      {step === 3 && form && card?.preview && (
        <Review card={card} form={form} cv={cluster} script={script} onScript={setScript} problems={problems} slurmOn={slurmOn} pending={pending} error={error} saveAs={saveAs} onSaveAs={setSaveAs} onBack={() => go(2)} onLaunch={launch} />
      )}
    </div>
  );
}
