"use client";

import { useState, useTransition } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Plus } from "lucide-react";
import { deleteTemplateAction } from "@/app/actions";
import { Notice } from "@/components/models/ui";
import { Pill } from "@/components/overview/ui";
import { testedSentence, type RecipeCard } from "@/components/recipes/recipe-card";
import { TemplateEditor } from "@/components/recipes/template-editor";
import { engineName } from "@/components/deployments/new/picker";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { deploymentHref } from "@/lib/deployments-model";
import type { ManagedModelSpec, ModelRoute } from "@/lib/obleth";
import { cn } from "@/lib/utils";

const COLS = "grid-cols-[minmax(0,1.7fr)_90px_minmax(0,1.2fr)_minmax(0,1fr)_190px]";

/** What one replica of a recipe asks for, in a line. */
export function recipeAsks(c: RecipeCard): string {
  const p = c.preview;
  if (!p) return "—";
  return [p.partition || "any partition", p.gres || null, p.cpusPerTask ? `${p.cpusPerTask} CPU` : null, p.mem || null].filter(Boolean).join(" · ");
}

export function RecipesTab({ recipes, specs, models }: { recipes: RecipeCard[]; specs: ManagedModelSpec[]; models: ModelRoute[] }) {
  const router = useRouter();
  const { confirm, confirmElement } = useConfirm();
  const [, start] = useTransition();
  const [editor, setEditor] = useState<{ open: boolean; initial?: { id?: string; name: string; body: string } }>({ open: false });
  const [notice, setNotice] = useState<string | null>(null);
  const names = new Map(models.map((m) => [m.id, m.model_name]));
  const launchedAs = (c: RecipeCard) =>
    specs.filter((s) => s.launcher_spec?.recipe_id === c.id || (c.recipeId && s.launcher_spec?.recipe_id === c.recipeId)).map((s) => names.get(s.model_id)).filter((n): n is string => !!n);

  function remove(c: RecipeCard) {
    start(async () => {
      const ok = await confirm({ title: `Delete ${c.name ?? c.id}?`, description: "Deployments launched from it keep running; you just can't launch it again.", confirmLabel: "Delete" });
      if (!ok || !c.recipeId) return;
      const res = await deleteTemplateAction(c.recipeId);
      if (!res.ok) setNotice(res.error);
      router.refresh();
    });
  }

  const saved = recipes.filter((c) => c.source === "db");
  const library = recipes.filter((c) => c.source === "file" && c.preview?.kind !== "engine");
  const engines = recipes.filter((c) => c.source === "file" && c.preview?.kind === "engine");

  const row = (c: RecipeCard) => {
    const launched = launchedAs(c);
    const toFill = c.preview?.inputs.filter((i) => i.required && !i.default).length ?? 0;
    const based = c.preview?.basedOn ? recipes.find((x) => x.id === c.preview?.basedOn)?.name ?? c.preview.basedOn : null;
    const sub = !c.valid ? `Can't be read: ${c.error}` : c.source === "db" ? [based ? `From ${based}` : null, c.preview?.model].filter(Boolean).join(" · ") : [c.preview?.model, c.preview?.tested?.[0] ? `served on ${c.preview.tested[0].hardware}` : "not run yet", toFill ? `${toFill} value${toFill === 1 ? "" : "s"} to fill in` : null].filter(Boolean).join(" · ");
    return (
      <div key={`${c.source}:${c.id}`} className={cn("grid min-h-[54px] items-center gap-3.5 border-t border-border px-[18px] py-2 text-[13px]", COLS)}>
        <span className="flex min-w-0 flex-col">
          <span className="truncate font-medium">{c.name ?? c.id}</span>
          <span className="truncate text-[11.5px] text-muted-foreground">{sub || c.description || c.apiModelName}</span>
        </span>
        <span className="truncate">{engineName(c.engine)}</span>
        <span className="truncate font-mono text-[12px]">{recipeAsks(c)}</span>
        <span className="flex min-w-0 flex-wrap gap-x-2">
          {launched.length ? launched.map((n) => <Link key={n} href={deploymentHref(n)} className="font-mono text-[12px] underline decoration-muted-foreground/40 underline-offset-[3px] hover:decoration-foreground">{n}</Link>) : <span className="text-[11.5px] text-muted-foreground">not launched</span>}
        </span>
        <span className="flex justify-end gap-3.5 text-[12.5px]">
          {c.valid && <Link href={`/deployments/new?recipe=${encodeURIComponent(c.id)}`} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">Launch</Link>}
          {c.source === "file" ? (
            <button type="button" onClick={() => setEditor({ open: true, initial: { name: `${c.name ?? c.id} (copy)`, body: c.body ?? "" } })} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">Copy to edit</button>
          ) : (
            <>
              <button type="button" onClick={() => setEditor({ open: true, initial: { id: c.recipeId, name: c.name ?? c.id, body: c.body ?? "" } })} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">Edit</button>
              <button type="button" onClick={() => remove(c)} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">Delete</button>
            </>
          )}
        </span>
      </div>
    );
  };

  const table = (label: string, title: string, note: string, items: RecipeCard[], empty: string) => (
    <section aria-label={label} className="overflow-hidden rounded-xl border border-border bg-card">
      <div className="flex flex-wrap items-baseline justify-between gap-2 px-[18px] py-3.5"><span className="text-sm font-semibold">{title} · {items.length}</span><span className="text-[12px] text-muted-foreground">{note}</span></div>
      <div className="overflow-x-auto">
        <div className="min-w-[900px]">
          <div className={cn("grid items-center gap-3.5 border-t border-border px-[18px] py-2.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground", COLS)}>
            <span>Recipe</span><span>Engine</span><span>Asks for</span><span>Launched as</span><span />
          </div>
          {items.length === 0 ? <p className="border-t border-border px-[18px] py-6 text-center text-[13px] text-muted-foreground">{empty}</p> : items.map(row)}
        </div>
      </div>
    </section>
  );

  return (
    <div className="flex flex-col gap-4">
      {confirmElement}
      {notice && <Notice strong onDismiss={() => setNotice(null)}>{notice}</Notice>}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-[13px] text-muted-foreground">A recipe is how a model is launched on Slurm, with the few settings someone launching it may change.</p>
        <Button type="button" variant="outline" size="sm" className="h-9" onClick={() => setEditor({ open: true })}><Plus className="h-4 w-4" />New recipe</Button>
      </div>
      {table("Saved recipes", "Saved", "Your team's: a library recipe or engine with the values that worked. Save one from a launch, or from a deployment that serves.", saved, "Nothing saved yet. Launch from the Library and tick \u201cAlso save these settings as a recipe\u201d, or save a deployment that serves from its page.")}
      {table("Library", "Library", "Shipped with obleth in its recipes folder, each with where it was run and served; copy one to change it.", library, "No recipes in the library folder.")}
      {engines.length > 0 && (
        <section aria-label="Engines" className="flex flex-col gap-3 rounded-xl border border-border bg-card px-[18px] py-4">
          <div className="flex flex-wrap items-baseline justify-between gap-2"><span className="text-sm font-semibold">Engines</span><span className="text-[12px] text-muted-foreground">For a Hugging Face model with no recipe yet; chosen after the model, from what its files need</span></div>
          <div className="grid gap-2.5 sm:grid-cols-2 xl:grid-cols-4">
            {engines.map((c) => (
              <div key={c.id} className="flex flex-col gap-1.5 rounded-lg border border-border bg-background/40 px-3.5 py-3">
                <span className="text-[13.5px] font-semibold">{c.name}</span>
                <span className="text-[12px] leading-relaxed text-muted-foreground">{c.description}</span>
                {c.preview?.tested?.[0] && <span className="text-[12px] leading-relaxed text-secondary-foreground">{testedSentence(c.preview.tested[0])}</span>}
                <span className="flex gap-3.5 pt-1 text-[12.5px]">
                  <Link href={`/deployments/new?recipe=${encodeURIComponent(c.id)}`} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">Launch a model</Link>
                  <button type="button" onClick={() => setEditor({ open: true, initial: { name: `${c.name ?? c.id} (copy)`, body: c.body ?? "" } })} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">Copy to edit</button>
                </span>
              </div>
            ))}
          </div>
        </section>
      )}
      <TemplateEditor
        key={editor.initial?.id ?? editor.initial?.name ?? "new"}
        open={editor.open}
        onOpenChange={(open) => setEditor((e) => ({ ...e, open }))}
        initial={editor.initial}
        onSaved={() => router.refresh()}
      />
    </div>
  );
}
