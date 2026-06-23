"use client";
import React, { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { type LauncherSpec } from "@/components/slurm-launcher/slurm-launcher";
import { recipeDefaults, type SlurmRecipe } from "@/lib/model-recipes";
import { type SavedRecipe } from "@/lib/obleth";

export function CatalogDrawer(props: {
  open: boolean;
  onClose: () => void;
  curated: readonly SlurmRecipe[];
  currentBackendId: string;
  currentSpec: () => LauncherSpec;
  onUse: (spec: LauncherSpec) => void;
}): React.ReactElement | null {
  const [recipeName, setRecipeName] = useState("");
  const queryClient = useQueryClient();

  const { data, isLoading, isError } = useQuery<SavedRecipe[]>({
    queryKey: ["recipes"],
    queryFn: () =>
      fetch("/api/live/recipes").then((r) => {
        if (!r.ok) throw new Error("failed");
        return r.json() as Promise<SavedRecipe[]>;
      }),
    enabled: props.open,
  });

  const saveMutation = useMutation({
    mutationFn: (name: string) =>
      fetch("/api/live/recipes", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name,
          backend: props.currentBackendId,
          author: "",
          spec: props.currentSpec(),
        }),
      }).then((r) => {
        if (!r.ok) throw new Error("save failed");
        return r.json();
      }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["recipes"] });
      setRecipeName("");
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (id: string) =>
      fetch(`/api/live/recipes/${id}`, { method: "DELETE" }).then((r) => {
        if (!r.ok) throw new Error("delete failed");
      }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["recipes"] });
    },
  });

  if (!props.open) return null;

  return (
    <Card className="absolute left-0 right-0 top-full z-50 mt-1 max-h-[70vh] overflow-y-auto border border-border bg-background shadow-lg">
      <CardHeader className="flex flex-row items-center justify-between pb-2">
        <CardTitle className="text-sm font-semibold">Catalog</CardTitle>
        <button
          type="button"
          onClick={props.onClose}
          className="text-xs text-muted-foreground hover:text-foreground"
          aria-label="Close catalog"
        >
          Close
        </button>
      </CardHeader>

      <CardContent className="space-y-5 pb-4">
        {/* Save current configuration */}
        <section className="space-y-2">
          <p className="text-xs font-medium text-muted-foreground uppercase tracking-wide">
            Save current configuration
          </p>
          <div className="flex gap-2">
            <Input
              value={recipeName}
              onChange={(e) => setRecipeName(e.target.value)}
              placeholder="Recipe name"
              className="h-8 text-xs"
            />
            <Button
              type="button"
              size="sm"
              className="h-8 text-xs shrink-0"
              disabled={!recipeName.trim() || saveMutation.isPending}
              onClick={() => saveMutation.mutate(recipeName.trim())}
            >
              {saveMutation.isPending ? "Saving…" : "Save"}
            </Button>
          </div>
          {saveMutation.isError && (
            <p className="text-xs text-destructive">Save failed.</p>
          )}
        </section>

        {/* Saved recipes */}
        <section className="space-y-2">
          <p className="text-xs font-medium text-muted-foreground uppercase tracking-wide">
            Saved recipes
          </p>
          {isLoading && (
            <p className="text-xs text-muted-foreground">Loading…</p>
          )}
          {isError && (
            <p className="text-xs text-destructive">Could not load recipes.</p>
          )}
          {!isLoading && !isError && (!data || data.length === 0) && (
            <p className="text-xs text-muted-foreground">No saved recipes yet.</p>
          )}
          {data && data.length > 0 && (
            <ul className="space-y-1">
              {data.map((recipe) => (
                <li
                  key={recipe.id}
                  className="flex items-center justify-between gap-2 rounded-md border border-border bg-muted/30 px-3 py-2"
                >
                  <div className="min-w-0">
                    <p className="truncate text-xs font-medium">{recipe.name}</p>
                    <p className="truncate text-[10px] text-muted-foreground">
                      {recipe.backend} · {recipe.author || "shared"}
                    </p>
                  </div>
                  <div className="flex shrink-0 gap-1">
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      className="h-7 text-[11px]"
                      onClick={() => props.onUse(recipe.spec as LauncherSpec)}
                    >
                      Use
                    </Button>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="h-7 text-[11px] text-muted-foreground hover:text-destructive"
                      disabled={deleteMutation.isPending}
                      onClick={() => deleteMutation.mutate(recipe.id)}
                    >
                      Delete
                    </Button>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </section>

        {/* Curated examples */}
        <section className="space-y-2">
          <p className="text-xs font-medium text-muted-foreground uppercase tracking-wide">
            Curated examples
          </p>
          <ul className="space-y-1">
            {props.curated.map((recipe) => (
              <li
                key={recipe.id}
                className="flex items-center justify-between gap-2 rounded-md border border-border bg-muted/30 px-3 py-2"
              >
                <div className="min-w-0">
                  <div className="flex items-center gap-1.5">
                    <p className="truncate text-xs font-medium">{recipe.label}</p>
                    {recipe.badge && (
                      <span className="rounded bg-muted px-1 py-0.5 text-[10px] text-muted-foreground">
                        {recipe.badge}
                      </span>
                    )}
                  </div>
                  {recipe.hint && (
                    <p className="truncate text-[10px] text-muted-foreground">
                      {recipe.hint}
                    </p>
                  )}
                </div>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="h-7 shrink-0 text-[11px]"
                  onClick={() =>
                    props.onUse({
                      backendId: recipe.id,
                      recipeValues: recipeDefaults(recipe),
                    })
                  }
                >
                  Use
                </Button>
              </li>
            ))}
          </ul>
        </section>
      </CardContent>
    </Card>
  );
}
