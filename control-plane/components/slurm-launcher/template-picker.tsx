"use client";
import React from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import {
  backendOf,
  resolveRecipe,
  type BackendId,
  type SlurmRecipe,
} from "@/lib/model-recipes";
import { type LauncherSpec } from "@/components/slurm-launcher/spec";
import { type SavedRecipe } from "@/lib/obleth";

// Step 3 of the launcher: choose a starting point for the selected backend.
// Folds the old "Catalog" in - a user's saved recipes sit alongside the built-in
// templates, and "Start from scratch" opens the bare backend form.
export function TemplatePicker(props: {
  backend: BackendId;
  templates: readonly SlurmRecipe[]; // built-in/file recipes for this backend
  allRecipes: readonly SlurmRecipe[]; // for resolving saved-recipe backends
  onUseTemplate: (recipe: SlurmRecipe) => void;
  onUseSaved: (spec: LauncherSpec) => void;
  onScratch: () => void;
}): React.ReactElement {
  const queryClient = useQueryClient();

  const { data: saved, isLoading } = useQuery<SavedRecipe[]>({
    queryKey: ["recipes"],
    queryFn: () =>
      fetch("/api/live/recipes").then((r) => {
        if (!r.ok) throw new Error("failed");
        return r.json() as Promise<SavedRecipe[]>;
      }),
  });

  const deleteMutation = useMutation({
    mutationFn: (id: string) =>
      fetch(`/api/live/recipes/${id}`, { method: "DELETE" }).then((r) => {
        if (!r.ok) throw new Error("delete failed");
      }),
    onSuccess: () =>
      void queryClient.invalidateQueries({ queryKey: ["recipes"] }),
  });

  // A saved recipe belongs to this backend if its stored family matches, or its
  // serialized recipe id resolves to a recipe in this family (legacy rows).
  const mine = (saved ?? []).filter((s) => {
    if (s.backend === props.backend) return true;
    const r = resolveRecipe(props.allRecipes, (s.spec as LauncherSpec)?.backendId);
    return r ? backendOf(r) === props.backend : false;
  });

  return (
    <div className="space-y-4">
      {mine.length > 0 && (
        <section className="space-y-2">
          <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
            Your recipes
          </p>
          <ul className="space-y-2">
            {mine.map((s) => (
              <li key={s.id}>
                <div className="flex items-center justify-between gap-2 rounded-lg border border-border p-3 hover:border-primary/60 hover:bg-muted/40">
                  <button
                    type="button"
                    className="min-w-0 flex-1 text-left"
                    onClick={() => props.onUseSaved(s.spec as LauncherSpec)}
                  >
                    <span className="block truncate text-sm font-medium">
                      {s.name}
                    </span>
                    <span className="block truncate text-xs text-muted-foreground">
                      {s.author ? s.author : "shared"}
                    </span>
                  </button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    className="h-7 shrink-0 text-[11px] text-muted-foreground hover:text-destructive"
                    disabled={deleteMutation.isPending}
                    onClick={() => deleteMutation.mutate(s.id)}
                  >
                    Delete
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="space-y-2">
        <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
          Built-in templates
        </p>
        {props.templates.length === 0 && (
          <p className="text-xs text-muted-foreground">
            No built-in templates for this backend.
          </p>
        )}
        <ul className="space-y-2">
          {props.templates.map((r) => (
            <li key={r.id}>
              <button
                type="button"
                onClick={() => props.onUseTemplate(r)}
                className="flex w-full items-center justify-between gap-2 rounded-lg border border-border p-3 text-left transition hover:border-primary/60 hover:bg-muted/40"
              >
                <div className="min-w-0">
                  <div className="flex items-center gap-1.5">
                    <span className="truncate text-sm font-medium">{r.label}</span>
                    {r.badge && (
                      <span className="rounded bg-primary/10 px-1.5 py-0.5 text-[10px] text-primary">
                        {r.badge}
                      </span>
                    )}
                  </div>
                  {r.hint && (
                    <span className="mt-0.5 block truncate text-xs text-muted-foreground">
                      {r.hint}
                    </span>
                  )}
                </div>
                <span className="shrink-0 text-xs font-medium text-muted-foreground">Use</span>
              </button>
            </li>
          ))}
        </ul>
      </section>

      <button
        type="button"
        onClick={props.onScratch}
        className="flex w-full items-center gap-2 rounded-lg border border-dashed border-border p-3 text-left text-sm text-muted-foreground transition hover:border-primary/60 hover:text-foreground"
      >
        <span className="text-base leading-none">+</span>
        Start from scratch
      </button>

      {isLoading && (
        <p className="text-xs text-muted-foreground">Loading saved recipes...</p>
      )}
    </div>
  );
}
