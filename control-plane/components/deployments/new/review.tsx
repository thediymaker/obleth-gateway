"use client";

import { useState } from "react";
import { Notice } from "@/components/models/ui";
import type { RecipeCard } from "@/components/recipes/recipe-card";
import { Button } from "@/components/ui/button";
import type { DeployForm, Problem } from "@/lib/deploy-form";
import { parseTimeLimit, walltimeLabel } from "@/lib/deployments-model";
import { renderScript, type ClusterValues } from "@/lib/recipe-inputs";
import { MODEL_TYPE_NAMES } from "@/lib/models-model";
import { cn } from "@/lib/utils";

/** The script as the job will run it, env exports included, for the review. */
export function reviewScript(body: string, card: RecipeCard, form: DeployForm, cv: ClusterValues): string {
  const p = card.preview!;
  const filled = renderScript(body, p.inputs, form.inputs, cv, { nodes: form.slurm.nodes, builtins: { api_model_name: form.name.trim() } });
  const env = Object.entries(form.env).filter(([k]) => k.trim());
  if (!env.length) return filled;
  const block = env.map(([k, v]) => `export ${k}='${v.replace(/'/g, "'\\''")}'`).join("\n");
  const lines = filled.split("\n");
  return lines[0]?.startsWith("#!") ? [lines[0], block, ...lines.slice(1)].join("\n") : [block, ...lines].join("\n");
}

export function Review({ card, form, cv, script, onScript, problems, slurmOn, pending, error, saveAs, onSaveAs, onBack, onLaunch }: {
  card: RecipeCard;
  form: DeployForm;
  cv: ClusterValues;
  script: string | null;
  onScript: (s: string | null) => void;
  problems: Problem[];
  slurmOn: boolean;
  pending: boolean;
  error: string | null;
  saveAs: { on: boolean; name: string };
  onSaveAs: (v: { on: boolean; name: string }) => void;
  onBack: () => void;
  onLaunch: () => void;
}) {
  const p = card.preview!;
  const [copied, setCopied] = useState(false);
  const s = form.slurm;
  const minutes = parseTimeLimit(s.time_limit);
  const target = form.serving.keep_running || 1;
  const shown = reviewScript(script ?? p.rawBody, card, form, cv);
  const sent = [`partition=${s.partition || "?"}`, s.account && `account=${s.account}`, s.qos && `qos=${s.qos}`, s.time_limit && `time=${s.time_limit}`, s.nodes > 1 && `nodes=${s.nodes}`, s.gres && `gres=${s.gres}`, s.cpus_per_task && `cpus-per-task=${s.cpus_per_task}`, s.mem && `mem=${s.mem}`, s.constraints && `constraint=${s.constraints}`, s.exclude && `exclude=${s.exclude}`, s.log_output_dir && `output=${s.log_output_dir}/`].filter(Boolean).join(" ");

  return (
    <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_380px]">
      <section aria-label="Job script" className="flex min-w-0 flex-col gap-3 rounded-xl border border-border bg-card px-5 py-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div><span className="text-sm font-semibold">Job script</span><p className="text-xs text-muted-foreground">What each replica&apos;s job runs, with your values filled in. obleth binds the port before it.</p></div>
          <span className="flex gap-2">
            <Button type="button" variant="outline" size="sm" onClick={() => { navigator.clipboard?.writeText(shown); setCopied(true); }}>{copied ? "Copied" : "Copy"}</Button>
            <Button type="button" variant="outline" size="sm" onClick={() => onScript(script === null ? p.rawBody : null)}>{script === null ? "Edit" : "Undo edits"}</Button>
          </span>
        </div>
        {script === null ? (
          <pre className="max-h-[560px] overflow-auto rounded-lg border border-border bg-background px-3 py-2.5 font-mono text-[12px] leading-relaxed text-secondary-foreground">
            <span className="text-muted-foreground"># obleth: binds the first free port in this replica&apos;s window and exports OBLETH_SERVING_PORT{"\n"}</span>
            {shown}
          </pre>
        ) : (
          <>
            <textarea aria-label="Job script" value={script} onChange={(e) => onScript(e.target.value)} rows={22} spellCheck={false} className="w-full rounded-lg border border-input bg-background px-3 py-2.5 font-mono text-[12px] leading-relaxed focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring" />
            <p className="text-[12px] text-muted-foreground">You&apos;re editing the recipe&apos;s script for this launch; {"{{inputs}}"} and {"{{cluster.*}}"} are still filled in.</p>
          </>
        )}
        <div className="flex flex-col gap-1 border-t border-border pt-3">
          <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Sent to Slurm as</span>
          <p className="break-all font-mono text-[11.5px] leading-relaxed text-secondary-foreground">{sent}</p>
        </div>
      </section>

      <aside aria-label="Launch" className="flex flex-col gap-4 rounded-xl border border-border bg-card px-5 py-5">
        <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">When you launch</span>
        <ol className="flex flex-col gap-3 text-[13px] leading-relaxed text-secondary-foreground">
          <li className="grid grid-cols-[20px_minmax(0,1fr)] gap-2"><span className="font-mono text-muted-foreground">1</span><span>Adds <span className="font-mono">{form.name || "…"}</span> to Models ({MODEL_TYPE_NAMES[p.modelType] ?? p.modelType}). Requests get 503 until a replica is healthy.</span></li>
          <li className="grid grid-cols-[20px_minmax(0,1fr)] gap-2"><span className="font-mono text-muted-foreground">2</span><span>Submits {target} job{target === 1 ? "" : "s"} to {s.partition}{s.nodes > 1 ? `, ${s.nodes} nodes each` : ""}{s.account ? ` as ${s.account}${s.qos ? ` / ${s.qos}` : ""}` : ""}{minutes ? `, ${walltimeLabel(minutes)} each` : ""}.</span></li>
          <li className="grid grid-cols-[20px_minmax(0,1fr)] gap-2"><span className="font-mono text-muted-foreground">3</span><span>Checks <span className="font-mono">{form.serving.health_path}</span> while the model loads, then warms it up with a one-token request.</span></li>
          <li className="grid grid-cols-[20px_minmax(0,1fr)] gap-2"><span className="font-mono text-muted-foreground">4</span><span>Keeps {target} running: replaces a job that ends{form.serving.stop_after_failed_launches > 0 ? `, and stops after ${form.serving.stop_after_failed_launches} failed launches in a row` : ""}.</span></li>
        </ol>
        <div className="flex flex-col gap-2 rounded-lg border border-border px-3.5 py-3">
          <label className="flex items-center gap-2 text-[13px] font-medium"><input type="checkbox" checked={saveAs.on} onChange={(e) => onSaveAs({ ...saveAs, on: e.target.checked })} className="h-4 w-4 accent-foreground" />Also save these settings as a recipe</label>
          {saveAs.on && <input aria-label="Saved recipe name" value={saveAs.name} onChange={(e) => onSaveAs({ ...saveAs, name: e.target.value })} className="h-9 rounded-lg border border-input bg-background px-3 text-[13px]" />}
          <span className="text-[11.5px] text-muted-foreground">{saveAs.on ? "It appears under Saved, ready to launch the same way." : "Or save it later from the deployment's page, once it serves."}</span>
        </div>
        <div className={cn("border-t border-border pt-3 text-[13px]")}>
          {problems.length === 0 ? <span>All checks passed.</span> : problems.map((x) => <p key={x.path + x.message} className="font-medium">! {x.message}</p>)}
        </div>
        {error && <Notice strong>{error}</Notice>}
        <div className="flex-1" />
        <div className="flex gap-2">
          <Button type="button" variant="outline" className="h-10" onClick={onBack}>Back</Button>
          <Button type="button" className="h-10 flex-1" disabled={pending || !slurmOn || problems.length > 0} onClick={onLaunch}>{pending ? "Launching…" : `Launch ${target} replica${target === 1 ? "" : "s"}`}</Button>
        </div>
      </aside>
    </div>
  );
}
