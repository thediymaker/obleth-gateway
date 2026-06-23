"use client";
import React, { useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  buildRecipeCommand,
  buildRecipePreamble,
  recipeDefaults,
  SLURM_RECIPES,
  type SlurmRecipe,
} from "@/lib/model-recipes";
import {
  ResourceFields,
  type ResourceValue,
} from "@/components/slurm-launcher/resource-fields";
import { useClusterResources } from "@/components/slurm-launcher/use-cluster-resources";
import { PerformanceFields } from "@/components/slurm-launcher/performance-fields";

export type LauncherSubmit = (
  formData: FormData,
) => Promise<{ ok: boolean; error?: string }>;

// MODEL_TYPE_OPTIONS is not exported from model-manager.tsx, so we keep a local
// minimal list of the served model types.
const MODEL_TYPE_OPTIONS = [
  { value: "chat", label: "Chat / completions" },
  { value: "embedding", label: "Embeddings" },
  { value: "completion", label: "Completion" },
] as const;

const EMPTY_RESOURCES: ResourceValue = {
  partition: "",
  node: "",
  gres: "",
  cpusPerTask: "",
  mem: "",
};

export function SlurmLauncher(props: {
  mode: "create" | "edit";
  recipes?: readonly SlurmRecipe[];
  onSubmit: LauncherSubmit;
  onCancel?: () => void;
  busy?: boolean;
}): React.ReactElement {
  const recipes = props.recipes ?? SLURM_RECIPES;

  const [modelName, setModelName] = useState("");
  const [modelType, setModelType] = useState("chat");
  const [backendId, setBackendId] = useState(recipes[0]?.id ?? "");
  const recipe = recipes.find((r) => r.id === backendId) ?? recipes[0];
  const isCustom = recipe.manual === true;

  const [model, setModel] = useState("");
  const [port, setPort] = useState("8000");
  const [recipeValues, setRecipeValues] = useState<Record<string, string>>(
    recipeDefaults(recipe),
  );
  const [preamble, setPreamble] = useState("");
  const [resources, setResources] = useState<ResourceValue>(EMPTY_RESOURCES);
  const [vramGb, setVramGb] = useState("");
  const [nodes, setNodes] = useState("1");
  const [replicas, setReplicas] = useState("2");
  const [healthPath, setHealthPath] = useState(recipe.healthPath);
  const [maxJobFailures, setMaxJobFailures] = useState("0");
  const [image, setImage] = useState("");
  const [logOutputDir, setLogOutputDir] = useState("");
  const [account, setAccount] = useState("");
  const [qos, setQos] = useState("");
  const [timeLimit, setTimeLimit] = useState("");
  const [constraints, setConstraints] = useState("");
  const [exclude, setExclude] = useState("");
  const [scriptBody, setScriptBody] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [showAdvancedPlacement, setShowAdvancedPlacement] = useState(false);

  const { data: resourcesData } = useClusterResources();

  // Derived launch script — reuse the recipe engine verbatim.
  const generatedCmd = buildRecipeCommand(recipe, {
    model,
    port,
    upstreamModel: modelName,
    values: recipeValues,
  });
  const recipePreamble = buildRecipePreamble(recipe, recipeValues);
  const effectivePreamble = [recipePreamble, preamble.trim()]
    .filter(Boolean)
    .join("\n");

  function selectBackend(id: string) {
    if (id === backendId) return;
    setBackendId(id);
    const next = recipes.find((r) => r.id === id) ?? recipes[0];
    setRecipeValues(recipeDefaults(next));
    setHealthPath(next.healthPath);
  }

  function setRecipeValue(id: string, value: string) {
    setRecipeValues((current) => ({ ...current, [id]: value }));
  }

  async function launch() {
    setError(null);
    if (!modelName.trim()) return setError("Model name is required.");
    if (!resources.partition.trim()) return setError("Partition is required.");
    if (isCustom ? !scriptBody.trim() : !model.trim())
      return setError(
        isCustom ? "Enter the job script." : "Enter the model path/id.",
      );

    const fd = new FormData();
    // Model identity.
    fd.set("model_name", modelName.trim());
    fd.set("model_type", modelType);
    fd.set("upstream_model", modelName.trim());
    fd.set("context_window", recipe.params?.some((p) => p.id === "ctx_size")
      ? recipeValues.ctx_size ?? ""
      : "");

    // Slurm envelope.
    fd.set("endpoint_mode", "slurm");
    fd.set("slurm_partition", resources.partition);
    fd.set("slurm_gres", resources.gres);
    fd.set("slurm_nodes", nodes);
    fd.set("slurm_image", image);
    fd.set("slurm_preamble", effectivePreamble);
    fd.set("slurm_log_output_dir", logOutputDir);
    fd.set("slurm_launch_command", isCustom ? "" : generatedCmd);
    fd.set("slurm_script_body", isCustom ? scriptBody : "");
    fd.set("slurm_cpus_per_task", resources.cpusPerTask);
    fd.set("slurm_mem", resources.mem);
    fd.set("slurm_serving_port", port);
    fd.set("slurm_health_path", healthPath || recipe.healthPath || "/health");
    fd.set("slurm_target_replicas", replicas);
    fd.set("slurm_max_job_failures", maxJobFailures);
    fd.set("slurm_account", account);
    fd.set("slurm_qos", qos);
    fd.set("slurm_time_limit", timeLimit);
    fd.set("slurm_constraints", constraints);
    fd.set("slurm_exclude", exclude);

    const res = await props.onSubmit(fd);
    if (!res.ok) setError(res.error ?? "Launch failed.");
  }

  const modelLabel = recipe.modelLabel ?? "Model handle";
  const previewBody = isCustom ? scriptBody : generatedCmd;

  return (
    <Card>
      <CardHeader>
        <CardTitle>
          {props.mode === "edit" ? "Edit managed model" : "Launch a model on Slurm"}
        </CardTitle>
        <Tabs value={backendId} onValueChange={selectBackend} className="mt-2">
          <TabsList>
            {recipes.map((r) => (
              <TabsTrigger key={r.id} value={r.id}>
                <span>{r.label}</span>
                {r.badge && (
                  <span className="rounded bg-muted px-1 py-0.5 text-[10px] text-muted-foreground">
                    {r.badge}
                  </span>
                )}
              </TabsTrigger>
            ))}
          </TabsList>
        </Tabs>
        {recipe.hint && (
          <p className="text-xs text-muted-foreground">{recipe.hint}</p>
        )}
      </CardHeader>

      <CardContent className="space-y-5">
        {/* Identity */}
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="space-y-1.5">
            <Label htmlFor="sl-model-name">Model name (API id)</Label>
            <Input
              id="sl-model-name"
              value={modelName}
              onChange={(e) => setModelName(e.target.value)}
              placeholder="my-model"
              className="h-9 text-xs"
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="sl-model-type">Model type</Label>
            <Select
              id="sl-model-type"
              value={modelType}
              onChange={(e) => setModelType(e.target.value)}
              className="h-9 text-xs"
            >
              {MODEL_TYPE_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
            </Select>
          </div>
        </div>

        {/* Resources */}
        <ResourceFields
          value={resources}
          onChange={setResources}
          resources={resourcesData}
        />
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="space-y-1.5">
            <Label htmlFor="sl-nodes">Node count</Label>
            <Input
              id="sl-nodes"
              type="number"
              value={nodes}
              onChange={(e) => setNodes(e.target.value)}
              placeholder="1"
              className="h-9 text-xs"
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="sl-vram">GPU VRAM (GB)</Label>
            <Input
              id="sl-vram"
              type="number"
              value={vramGb}
              onChange={(e) => setVramGb(e.target.value)}
              placeholder="96"
              className="h-9 text-xs"
            />
            <p className="text-xs text-muted-foreground">
              Discovery can&apos;t detect this — enter it to power the tuning
              recommendation.
            </p>
          </div>
        </div>

        {/* Model handle + image */}
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="space-y-1.5">
            <Label htmlFor="sl-model">{modelLabel}</Label>
            <Input
              id="sl-model"
              value={model}
              onChange={(e) => setModel(e.target.value)}
              placeholder={recipe.modelPlaceholder}
              className="h-9 text-xs"
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
            />
            {recipe.modelHint && (
              <p className="text-xs text-muted-foreground">{recipe.modelHint}</p>
            )}
          </div>
          {!isCustom && (
            <div className="space-y-1.5">
              <Label htmlFor="sl-image">
                Apptainer image{recipe.imageOptional ? " (optional)" : ""}
              </Label>
              <Input
                id="sl-image"
                value={image}
                onChange={(e) => setImage(e.target.value)}
                placeholder={recipe.imagePlaceholder}
                className="h-9 text-xs"
                autoCapitalize="none"
                autoCorrect="off"
                spellCheck={false}
              />
              {recipe.imageHint && (
                <p className="text-xs text-muted-foreground">{recipe.imageHint}</p>
              )}
            </div>
          )}
        </div>

        {/* Performance knobs OR custom script */}
        {isCustom ? (
          <div className="space-y-1.5">
            <Label htmlFor="sl-script">Job script</Label>
            <textarea
              id="sl-script"
              value={scriptBody}
              onChange={(e) => setScriptBody(e.target.value)}
              rows={10}
              spellCheck={false}
              className="flex w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
              placeholder="#!/bin/bash&#10;srun ..."
            />
          </div>
        ) : (
          <PerformanceFields
            recipe={recipe}
            values={recipeValues}
            onChange={setRecipeValue}
            vramGb={vramGb ? Number(vramGb) : null}
          />
        )}

        {/* Operator preamble */}
        {!isCustom && (
          <div className="space-y-1.5">
            <Label htmlFor="sl-preamble">Extra preamble (shell lines)</Label>
            <textarea
              id="sl-preamble"
              value={preamble}
              onChange={(e) => setPreamble(e.target.value)}
              rows={3}
              spellCheck={false}
              className="flex w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
              placeholder="module load cuda"
            />
          </div>
        )}

        {/* Live preview */}
        <div className="space-y-1.5">
          <Label>Script preview</Label>
          <pre className="max-h-48 overflow-auto whitespace-pre-wrap rounded-md border border-border bg-muted/40 px-3 py-2 font-mono text-xs text-muted-foreground">
            {[effectivePreamble, previewBody].filter(Boolean).join("\n\n") ||
              "(nothing yet)"}
          </pre>
        </div>

        {/* obleth-owned operational fields */}
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          <div className="space-y-1.5">
            <Label htmlFor="sl-port">Serving port</Label>
            <Input
              id="sl-port"
              type="number"
              value={port}
              onChange={(e) => setPort(e.target.value)}
              placeholder="8000"
              className="h-9 text-xs"
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="sl-health">Health path</Label>
            <Input
              id="sl-health"
              value={healthPath}
              onChange={(e) => setHealthPath(e.target.value)}
              placeholder="/health"
              className="h-9 text-xs"
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="sl-replicas">Target replicas</Label>
            <Input
              id="sl-replicas"
              type="number"
              value={replicas}
              onChange={(e) => setReplicas(e.target.value)}
              placeholder="2"
              className="h-9 text-xs"
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="sl-failures">Max job failures</Label>
            <Input
              id="sl-failures"
              type="number"
              value={maxJobFailures}
              onChange={(e) => setMaxJobFailures(e.target.value)}
              placeholder="0"
              className="h-9 text-xs"
            />
          </div>
        </div>

        {/* Advanced placement */}
        <div className="space-y-3">
          <button
            type="button"
            onClick={() => setShowAdvancedPlacement((v) => !v)}
            className="text-xs font-medium text-muted-foreground hover:text-foreground"
          >
            {showAdvancedPlacement ? "▾" : "▸"} Advanced placement
          </button>
          {showAdvancedPlacement && (
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
              <div className="space-y-1.5">
                <Label htmlFor="sl-account">Account</Label>
                <Input
                  id="sl-account"
                  value={account}
                  onChange={(e) => setAccount(e.target.value)}
                  className="h-9 text-xs"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="sl-qos">QoS</Label>
                <Input
                  id="sl-qos"
                  value={qos}
                  onChange={(e) => setQos(e.target.value)}
                  className="h-9 text-xs"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="sl-time">Time limit</Label>
                <Input
                  id="sl-time"
                  value={timeLimit}
                  onChange={(e) => setTimeLimit(e.target.value)}
                  placeholder="04:00:00"
                  className="h-9 text-xs"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="sl-constraints">Constraints</Label>
                <Input
                  id="sl-constraints"
                  value={constraints}
                  onChange={(e) => setConstraints(e.target.value)}
                  className="h-9 text-xs"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="sl-exclude">Exclude</Label>
                <Input
                  id="sl-exclude"
                  value={exclude}
                  onChange={(e) => setExclude(e.target.value)}
                  className="h-9 text-xs"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="sl-logdir">Log output dir</Label>
                <Input
                  id="sl-logdir"
                  value={logOutputDir}
                  onChange={(e) => setLogOutputDir(e.target.value)}
                  className="h-9 text-xs"
                />
              </div>
            </div>
          )}
        </div>

        {error && <p className="text-xs text-destructive">{error}</p>}

        {/* Footer */}
        <div className="flex items-center justify-end gap-2">
          {props.onCancel && (
            <Button
              type="button"
              variant="outline"
              onClick={props.onCancel}
              disabled={props.busy}
            >
              Cancel
            </Button>
          )}
          <Button type="button" onClick={launch} disabled={props.busy}>
            {props.busy ? "Launching…" : "Launch"}
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}
