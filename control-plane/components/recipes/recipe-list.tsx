"use client";

import { useState, useTransition, type ComponentType, type ReactNode } from "react";
import {
  AlertTriangle,
  CheckCircle2,
  ChevronRight,
  Clock,
  Cpu,
  FileText,
  HardDrive,
  Layers,
  Rocket,
  Server,
  SlidersHorizontal,
  Terminal,
} from "lucide-react";
import { deployRecipeAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
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

function compactJoin(parts: Array<string | number | null | undefined>): string {
  return parts
    .filter((part) => part !== null && part !== undefined && String(part).trim() !== "")
    .map(String)
    .join(" / ");
}

function plural(value: number, singular: string, pluralValue = `${singular}s`) {
  return `${value} ${value === 1 ? singular : pluralValue}`;
}

function shortPlacement(p?: RecipeDeployPreview): string {
  if (!p) return "Placement unavailable";
  return compactJoin([p.partition || "No partition", p.gres, p.mem, p.timeLimit]) || "No placement";
}

function scriptLineCount(script: string): number {
  if (!script.trim()) return 0;
  return script.split("\n").length;
}

function RecipeMetaBadge({ children }: { children: ReactNode }) {
  return <Badge className="border-border bg-background/80 text-[10px] text-muted-foreground">{children}</Badge>;
}

function MetricTile({
  icon: Icon,
  label,
  value,
}: {
  icon: ComponentType<{ className?: string }>;
  label: string;
  value: string;
}) {
  return (
    <div className="rounded-md border border-border/70 bg-background/35 px-3 py-2.5">
      <div className="flex items-center gap-2 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
        <Icon className="h-3.5 w-3.5" />
        {label}
      </div>
      <p className="mt-1 truncate text-sm font-medium" title={value}>
        {value}
      </p>
    </div>
  );
}

function DetailRow({ label, value }: { label: string; value?: string | null }) {
  return (
    <div className="grid grid-cols-[7.5rem_minmax(0,1fr)] gap-3 border-b border-border/50 py-2.5 last:border-0">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className={cn("min-w-0 break-words text-sm font-medium", !value && "text-muted-foreground")}>
        {value || "Not set"}
      </dd>
    </div>
  );
}

function OptionalDetailRow({ label, value }: { label: string; value?: string | null }) {
  if (!value) return null;
  return <DetailRow label={label} value={value} />;
}

/** A deploy-time editable override, pre-filled from the recipe. */
function OverrideField({
  label,
  value,
  onChange,
  placeholder,
  type = "text",
  disabled,
  className,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  type?: string;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <div className={cn("space-y-1", className)}>
      <Label className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">{label}</Label>
      <Input
        type={type}
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
        className="h-8 text-sm"
      />
    </div>
  );
}

function RecipeStatus({ recipe }: { recipe: RecipeCard }) {
  if (!recipe.valid || !recipe.preview) {
    return (
      <Badge className="gap-1.5 border-destructive/40 bg-destructive/10 text-[10px] text-destructive">
        <AlertTriangle className="h-3 w-3" />
        Invalid
      </Badge>
    );
  }
  if (recipe.warnings.length > 0) {
    return (
      <Badge className="gap-1.5 border-amber-500/35 bg-amber-500/10 text-[10px] text-amber-300">
        <AlertTriangle className="h-3 w-3" />
        Warnings
      </Badge>
    );
  }
  return (
    <Badge className="gap-1.5 border-emerald-500/35 bg-emerald-500/10 text-[10px] text-emerald-300">
      <CheckCircle2 className="h-3 w-3" />
      Ready
    </Badge>
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
  const [overrides, setOverrides] = useState({ qos: "", partition: "", timeLimit: "", replicas: "" });
  const [pending, startTransition] = useTransition();
  const [error, setError] = useState<string | null>(null);

  // Open a recipe and seed the editable override fields from its deploy preview.
  function openRecipe(recipe: RecipeCard) {
    setError(null);
    setSelected(recipe);
    const p = recipe.preview;
    setOverrides({
      qos: p?.qos ?? "",
      partition: p?.partition ?? "",
      timeLimit: p?.timeLimit ?? "",
      replicas: String(p?.targetReplicas ?? recipe.targetReplicas ?? 2),
    });
  }

  function confirmDeploy() {
    if (!selected) return;
    setError(null);
    const replicasNum = Number.parseInt(overrides.replicas, 10);
    startTransition(async () => {
      const res = await deployRecipeAction(selected.id, {
        api_model_name: selected.apiModelName,
        target_replicas:
          Number.isFinite(replicasNum) && replicasNum > 0 ? replicasNum : selected.targetReplicas,
        qos: overrides.qos,
        time_limit: overrides.timeLimit,
        partition: overrides.partition,
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
      <div className="rounded-lg border border-dashed border-border/70 bg-background/30 px-6 py-10 text-center">
        <FileText className="mx-auto h-8 w-8 text-muted-foreground/70" />
        <p className="mt-3 text-sm font-medium">No recipes found</p>
        <p className="mx-auto mt-1 max-w-md text-sm leading-relaxed text-muted-foreground">
          Drop a <code className="rounded bg-secondary px-1 py-0.5 text-xs">*.recipe</code> file into
          the recipes directory, then refresh this page.
        </p>
      </div>
    );
  }

  const preview = selected?.preview;

  return (
    <>
      <div className="overflow-hidden rounded-lg border border-border/70 bg-card/45">
        <div className="hidden grid-cols-[minmax(0,1fr)_8.5rem_8rem_minmax(14rem,0.7fr)_2.75rem] border-b border-border/70 bg-background/35 px-4 py-2.5 text-xs font-medium text-muted-foreground md:grid">
          <div>Recipe</div>
          <div>Engine</div>
          <div>Replicas</div>
          <div>Placement</div>
          <div />
        </div>
        <div className="divide-y divide-border/60">
          {recipes.map((recipe) => {
            const deployable = recipe.valid && Boolean(recipe.preview);
            const meta = compactJoin([recipe.engine, recipe.modelType, recipe.apiModelName]);
            const replicaLabel = recipe.preview
              ? plural(recipe.preview.targetReplicas, "replica")
              : recipe.targetReplicas
                ? plural(recipe.targetReplicas, "replica")
                : "Not set";

            return (
              <button
                key={recipe.id}
                type="button"
                disabled={!deployable}
                onClick={() => openRecipe(recipe)}
                className={cn(
                  "group grid w-full gap-3 px-4 py-4 text-left transition-colors md:grid-cols-[minmax(0,1fr)_8.5rem_8rem_minmax(14rem,0.7fr)_2.75rem] md:items-center",
                  deployable
                    ? "bg-card/30 hover:bg-muted/20 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                    : "cursor-not-allowed bg-background/30 opacity-75",
                )}
                aria-label={`Deploy recipe ${recipe.name ?? recipe.id}`}
              >
                <div className="min-w-0">
                  <div className="flex min-w-0 flex-wrap items-center gap-2">
                    <span className="truncate text-sm font-semibold" title={recipe.name ?? recipe.id}>
                      {recipe.name ?? recipe.id}
                    </span>
                    <RecipeStatus recipe={recipe} />
                  </div>
                  {meta && (
                    <p className="mt-1 truncate font-mono text-[11px] text-muted-foreground" title={meta}>
                      {meta}
                    </p>
                  )}
                  {recipe.description && (
                    <p className="mt-1.5 line-clamp-2 text-xs leading-relaxed text-muted-foreground">
                      {recipe.description}
                    </p>
                  )}
                  {!deployable && recipe.error && (
                    <p className="mt-1.5 text-xs leading-relaxed text-destructive">{recipe.error}</p>
                  )}
                  <div className="mt-3 flex flex-wrap gap-1.5 md:hidden">
                    {recipe.engine && <RecipeMetaBadge>{recipe.engine}</RecipeMetaBadge>}
                    {recipe.modelType && <RecipeMetaBadge>{recipe.modelType}</RecipeMetaBadge>}
                    <RecipeMetaBadge>{replicaLabel}</RecipeMetaBadge>
                    <RecipeMetaBadge>{shortPlacement(recipe.preview)}</RecipeMetaBadge>
                  </div>
                </div>

                <div className="hidden min-w-0 md:block">
                  <p className="truncate text-sm font-medium" title={recipe.engine || "Not set"}>
                    {recipe.engine || "Not set"}
                  </p>
                  {recipe.modelType && <p className="mt-0.5 text-[11px] text-muted-foreground">{recipe.modelType}</p>}
                </div>

                <div className="hidden min-w-0 md:block">
                  <p className="text-sm font-medium tabular-nums">{replicaLabel}</p>
                  {recipe.preview && (
                    <p className="mt-0.5 text-[11px] text-muted-foreground">
                      max {recipe.preview.maxJobFailures} failures
                    </p>
                  )}
                </div>

                <div className="hidden min-w-0 md:block">
                  <p className="truncate text-sm text-muted-foreground" title={shortPlacement(recipe.preview)}>
                    {shortPlacement(recipe.preview)}
                  </p>
                </div>

                <div className="hidden justify-end md:flex">
                  <span
                    className={cn(
                      "flex h-8 w-8 items-center justify-center rounded-md border border-border/70 bg-background/45 text-muted-foreground transition-colors",
                      deployable && "group-hover:border-border group-hover:text-foreground",
                    )}
                  >
                    <ChevronRight className="h-4 w-4" />
                  </span>
                </div>
              </button>
            );
          })}
        </div>
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
        <DialogContent className="grid max-h-[75vh] w-[min(1120px,calc(100vw-2rem))] max-w-none grid-rows-[auto_minmax(0,1fr)_auto] gap-0 overflow-hidden p-0">
          {selected && preview && (
            <>
              <DialogHeader className="border-b border-border/70 bg-background/35 px-6 py-4 pr-12">
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div className="min-w-0">
                    <p className="mb-1 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
                      Recipe deployment
                    </p>
                    <DialogTitle className="truncate text-lg">
                      Deploy &ldquo;{selected.name ?? selected.id}&rdquo;
                    </DialogTitle>
                    <DialogDescription className="mt-1 max-w-3xl">
                      Creates a managed route, starts{" "}
                      {plural(Number.parseInt(overrides.replicas, 10) || preview.targetReplicas, "replica")} on
                      Slurm, and promotes healthy replicas into rotation.
                    </DialogDescription>
                  </div>
                  <RecipeStatus recipe={selected} />
                </div>
              </DialogHeader>

              <div className="min-h-0 overflow-y-auto px-5 py-4 sm:px-6">
                <div className="grid gap-5 xl:grid-cols-[21rem_minmax(0,1fr)]">
                  <aside className="space-y-3.5">
                    <div className="rounded-lg border border-border/70 bg-background/30 p-3.5">
                      <div className="flex items-center gap-2 text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
                        <SlidersHorizontal className="h-3.5 w-3.5" />
                        Deploy overrides
                      </div>
                      <p className="mt-1 text-[11px] leading-relaxed text-muted-foreground">
                        Pre-filled from the recipe. Changes apply to this deployment only — the recipe
                        file is unchanged.
                      </p>
                      <div className="mt-3 grid grid-cols-2 gap-3">
                        <OverrideField
                          label="Partition"
                          value={overrides.partition}
                          onChange={(v) => setOverrides((o) => ({ ...o, partition: v }))}
                          placeholder="e.g. arm"
                          disabled={pending}
                        />
                        <OverrideField
                          label="QoS"
                          value={overrides.qos}
                          onChange={(v) => setOverrides((o) => ({ ...o, qos: v }))}
                          placeholder="default"
                          disabled={pending}
                        />
                        <OverrideField
                          label="Replicas"
                          type="number"
                          value={overrides.replicas}
                          onChange={(v) => setOverrides((o) => ({ ...o, replicas: v }))}
                          disabled={pending}
                        />
                        <OverrideField
                          label="Time limit"
                          value={overrides.timeLimit}
                          onChange={(v) => setOverrides((o) => ({ ...o, timeLimit: v }))}
                          placeholder="e.g. 04:00:00"
                          disabled={pending}
                        />
                      </div>
                    </div>

                    <div className="grid grid-cols-2 gap-3">
                      <MetricTile
                        icon={Server}
                        label="Route"
                        value={`${preview.apiModelName} (${preview.modelType})`}
                      />
                      <MetricTile icon={Layers} label="GPU" value={preview.gres || "Not set"} />
                      <MetricTile
                        icon={Cpu}
                        label="CPU"
                        value={preview.cpusPerTask != null ? `${preview.cpusPerTask}` : "Not set"}
                      />
                      <MetricTile icon={HardDrive} label="Memory" value={preview.mem || "Not set"} />
                    </div>

                    <dl className="rounded-lg border border-border/70 bg-background/30 px-4 py-2">
                      <DetailRow label="Engine" value={preview.engine} />
                      <DetailRow label="Service" value={`:${preview.port}${preview.healthPath}`} />
                      <DetailRow
                        label="Reliability"
                        value={`max ${plural(preview.maxJobFailures, "job failure")}`}
                      />
                      <OptionalDetailRow label="Account" value={preview.account} />
                      <OptionalDetailRow label="Constraints" value={preview.constraints} />
                      <OptionalDetailRow label="Exclude" value={preview.exclude} />
                      <OptionalDetailRow label="Log output" value={preview.logOutputDir} />
                    </dl>

                    {selected.warnings.length > 0 && (
                      <div className="rounded-md border border-amber-500/30 bg-amber-500/10 px-3 py-2.5 text-xs text-amber-200">
                        <div className="flex items-center gap-2 font-medium">
                          <AlertTriangle className="h-3.5 w-3.5" />
                          Recipe warnings
                        </div>
                        <p className="mt-1 leading-relaxed text-amber-200/80">
                          Not applied: {selected.warnings.join(", ")}
                        </p>
                      </div>
                    )}

                    {error && (
                      <div className="rounded-md border border-destructive/35 bg-destructive/10 px-3 py-2.5 text-sm text-destructive">
                        {error}
                      </div>
                    )}
                  </aside>

                  <section className="min-w-0">
                    <div className="flex flex-wrap items-center justify-between gap-2">
                      <div>
                        <p className="flex items-center gap-2 text-sm font-medium">
                          <Terminal className="h-4 w-4 text-muted-foreground" />
                          Launch script
                        </p>
                        <p className="mt-0.5 text-xs text-muted-foreground">
                          Body submitted to slurmrestd as shown; placement comes from the fields on the
                          left (slurmrestd ignores <code className="font-mono">#SBATCH</code> lines).
                        </p>
                      </div>
                      <Badge className="gap-1.5 bg-background text-[10px]">
                        <Clock className="h-3 w-3" />
                        {scriptLineCount(preview.scriptBody)} lines
                      </Badge>
                    </div>
                    <pre className="mt-3 max-h-[42vh] overflow-auto rounded-lg border border-border/70 bg-background/70 p-4 font-mono text-xs leading-relaxed shadow-inner">
                      {preview.scriptBody}
                    </pre>
                  </section>
                </div>
              </div>

              <DialogFooter className="border-t border-border/70 bg-background/35 px-6 py-3">
                <Button type="button" variant="ghost" onClick={() => setSelected(null)} disabled={pending}>
                  Cancel
                </Button>
                <Button type="button" onClick={confirmDeploy} disabled={pending}>
                  <Rocket className="h-4 w-4" />
                  {pending ? "Deploying..." : "Deploy recipe"}
                </Button>
              </DialogFooter>
            </>
          )}
        </DialogContent>
      </Dialog>
    </>
  );
}
