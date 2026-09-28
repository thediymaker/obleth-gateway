"use client";

import { useState, useTransition } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Plus } from "lucide-react";
import { deleteTemplateAction } from "@/app/actions";
import { Notice } from "@/components/models/ui";
import { Pill } from "@/components/overview/ui";
import type { RecipeCard } from "@/components/recipes/recipe-card";
import { TemplateEditor } from "@/components/recipes/template-editor";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { deploymentHref } from "@/lib/deployments-model";
import type { ManagedModelSpec, ModelRoute } from "@/lib/obleth";
import { cn } from "@/lib/utils";

const COLS = "grid-cols-[minmax(0,1.6fr)_80px_100px_minmax(0,1.3fr)_minmax(0,1fr)_190px]";

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

  return (
    <div className="flex flex-col gap-4">
      {confirmElement}
      {notice && <Notice strong onDismiss={() => setNotice(null)}>{notice}</Notice>}
      <div className="flex justify-end">
        <Button type="button" variant="outline" size="sm" className="h-9" onClick={() => setEditor({ open: true })}><Plus className="h-4 w-4" />New recipe</Button>
      </div>
      <section aria-label="Recipes" className="overflow-hidden rounded-xl border border-border bg-card">
        <div className="overflow-x-auto">
          <div className="min-w-[980px]">
            <div className={cn("grid items-center gap-3.5 px-[18px] py-2.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground", COLS)}>
              <span>Recipe</span><span>From</span><span>Engine</span><span>Asks for</span><span>Launched as</span><span />
            </div>
            {recipes.length === 0 && <p className="border-t border-border px-[18px] py-8 text-center text-[13px] text-muted-foreground">No recipes yet. Make one from a script you already run on the cluster.</p>}
            {recipes.map((c) => {
              const launched = launchedAs(c);
              const vars = c.preview?.variables?.filter((v) => v.required || !v.default).length ?? 0;
              return (
                <div key={`${c.source}:${c.id}`} className={cn("grid min-h-[54px] items-center gap-3.5 border-t border-border px-[18px] py-2 text-[13px]", COLS)}>
                  <span className="flex min-w-0 flex-col">
                    <span className="truncate font-medium">{c.name ?? c.id}</span>
                    <span className="truncate text-[11.5px] text-muted-foreground">
                      {!c.valid ? `Can't be read: ${c.error}` : [c.description, vars ? `${vars} value${vars === 1 ? "" : "s"} to fill in` : null].filter(Boolean).join(" · ") || c.apiModelName}
                    </span>
                  </span>
                  <span><Pill>{c.source === "file" ? "File" : "Saved"}</Pill></span>
                  <span className="truncate">{c.engine ?? "—"}</span>
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
            })}
          </div>
        </div>
        <p className="border-t border-border px-[18px] py-3 text-[12.5px] text-muted-foreground">File recipes come from the gateway&apos;s recipes folder and can&apos;t be changed here; copy one to make your own.</p>
      </section>
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
