"use client";

import { useState, useTransition } from "react";
import { deployRecipeAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
import type { RecipeCard, RecipeDeployPreview } from "./recipe-card";

function placementSummary(p: RecipeDeployPreview): string {
  return [
    p.partition,
    p.gres,
    p.cpusPerTask != null ? `${p.cpusPerTask}c` : null,
    p.mem,
    p.nodes != null ? `${p.nodes}n` : null,
    p.qos ? `qos:${p.qos}` : null,
    p.timeLimit ? `t:${p.timeLimit}` : null,
  ]
    .filter(Boolean)
    .join(" · ");
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="font-medium">{value}</dd>
    </>
  );
}

export function RecipeList({
  recipes,
  onDeployed,
}: {
  recipes: RecipeCard[];
  onDeployed?: () => void;
}) {
  const [selected, setSelected] = useState<RecipeCard | null>(null);
  const [pending, startTransition] = useTransition();
  const [error, setError] = useState<string | null>(null);

  function confirmDeploy() {
    if (!selected) return;
    setError(null);
    startTransition(async () => {
      const res = await deployRecipeAction(selected.id, {
        api_model_name: selected.apiModelName,
        target_replicas: selected.targetReplicas,
      });
      if (res.ok) {
        setSelected(null);
        onDeployed?.();
      } else {
        setError(res.error);
      }
    });
  }

  if (recipes.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        No recipes found. Drop a{" "}
        <code className="rounded bg-secondary px-1 py-0.5 text-xs">*.recipe</code> file into the
        recipes directory.
      </p>
    );
  }

  const preview = selected?.preview;

  return (
    <>
      <div className="grid gap-3">
        {recipes.map((r) => (
          <button
            key={r.id}
            type="button"
            disabled={!r.valid}
            onClick={() => {
              setError(null);
              setSelected(r);
            }}
            className={cn(
              "w-full rounded-lg border p-4 text-left transition-colors",
              r.valid
                ? "border-border/70 bg-background/40 hover:border-primary/50 hover:bg-accent"
                : "cursor-not-allowed border-destructive/40 bg-background/40 opacity-70",
            )}
          >
            <div className="flex items-center gap-2">
              <span className="font-medium">{r.name ?? r.id}</span>
              {!r.valid && <Badge className="border-destructive/40 text-destructive">Invalid</Badge>}
            </div>
            <div className="mt-0.5 text-xs text-muted-foreground">
              {[r.engine, r.modelType, r.apiModelName].filter(Boolean).join(" · ")}
            </div>
            {r.description && <p className="mt-1 text-sm text-muted-foreground">{r.description}</p>}
            {!r.valid && r.error && <p className="mt-1 text-sm text-destructive">{r.error}</p>}
          </button>
        ))}
      </div>

      <Dialog
        open={selected !== null}
        onOpenChange={(open) => {
          if (!open) {
            setSelected(null);
            setError(null);
          }
        }}
      >
        <DialogContent className="max-w-2xl">
          {selected && preview && (
            <>
              <DialogHeader>
                <DialogTitle>Deploy &ldquo;{selected.name ?? selected.id}&rdquo;</DialogTitle>
                <DialogDescription>
                  Creates a managed route and starts {preview.targetReplicas}{" "}
                  {preview.targetReplicas === 1 ? "replica" : "replicas"} on Slurm. obleth submits the
                  script below to slurmrestd and promotes healthy replicas into rotation.
                </DialogDescription>
              </DialogHeader>

              <div className="space-y-3 text-sm">
                <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5">
                  <Row label="Route" value={`${preview.apiModelName} (${preview.modelType})`} />
                  <Row label="Engine" value={preview.engine} />
                  <Row label="Service" value={`:${preview.port} ${preview.healthPath}`} />
                  <Row
                    label="Replicas"
                    value={`${preview.targetReplicas} (max ${preview.maxJobFailures} job failures)`}
                  />
                  <Row label="Placement" value={placementSummary(preview) || "—"} />
                </dl>

                {selected.warnings.length > 0 && (
                  <p className="text-xs text-amber-600">Not applied: {selected.warnings.join(", ")}</p>
                )}

                <div>
                  <p className="mb-1 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
                    Launch script
                  </p>
                  <pre className="max-h-64 overflow-auto rounded-md border border-border/70 bg-background/60 p-3 font-mono text-xs leading-relaxed">
                    {preview.scriptBody}
                  </pre>
                </div>

                {error && <p className="text-sm text-destructive">{error}</p>}
              </div>

              <DialogFooter>
                <Button type="button" variant="ghost" onClick={() => setSelected(null)} disabled={pending}>
                  Cancel
                </Button>
                <Button type="button" onClick={confirmDeploy} disabled={pending}>
                  {pending ? "Deploying…" : "Deploy"}
                </Button>
              </DialogFooter>
            </>
          )}
        </DialogContent>
      </Dialog>
    </>
  );
}
