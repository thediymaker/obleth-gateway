"use client";

import { useState, useTransition } from "react";
import { deployRecipeAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";

type RecipeCard = {
  id: string;
  valid: boolean;
  error?: string;
  name?: string;
  engine?: string;
  modelType?: string;
  description?: string;
  apiModelName?: string;
  targetReplicas?: number;
  warnings: string[];
};

export function RecipeList({ recipes }: { recipes: RecipeCard[] }) {
  const [pending, startTransition] = useTransition();
  const [msg, setMsg] = useState<string | null>(null);

  function deploy(r: RecipeCard) {
    setMsg(null);
    startTransition(async () => {
      const res = await deployRecipeAction(r.id, {
        api_model_name: r.apiModelName,
        target_replicas: r.targetReplicas,
      });
      setMsg(res.ok ? `Deployed ${r.name ?? r.id}` : `Error: ${res.error}`);
    });
  }

  if (recipes.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        No recipes found. Drop a <code className="rounded bg-secondary px-1 py-0.5 text-xs">*.recipe</code> file into the recipes directory.
      </p>
    );
  }

  return (
    <div className="space-y-3">
      {msg && <p className="text-sm">{msg}</p>}
      {recipes.map((r) => (
        <Card key={r.id}>
          <CardContent className="p-4">
            <div className="flex items-start justify-between gap-4">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  <span className="font-medium">{r.name ?? r.id}</span>
                  {!r.valid && (
                    <Badge className="border-destructive/40 text-destructive">
                      Invalid
                    </Badge>
                  )}
                </div>
                <div className="mt-0.5 text-xs text-muted-foreground">
                  {[r.engine, r.modelType, r.apiModelName].filter(Boolean).join(" · ")}
                </div>
                {r.description && (
                  <p className="mt-1 text-sm text-muted-foreground">{r.description}</p>
                )}
                {!r.valid && r.error && (
                  <p className="mt-1 text-sm text-destructive">{r.error}</p>
                )}
                {r.warnings.length > 0 && (
                  <p className="mt-1 text-xs text-amber-600">
                    Not applied: {r.warnings.join(", ")}
                  </p>
                )}
              </div>
              <Button
                variant="outline"
                size="sm"
                disabled={!r.valid || pending}
                onClick={() => deploy(r)}
              >
                {pending ? "Deploying…" : "Deploy"}
              </Button>
            </div>
          </CardContent>
        </Card>
      ))}
    </div>
  );
}
