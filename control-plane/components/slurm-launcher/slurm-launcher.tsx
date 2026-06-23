"use client";

import React, { useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import {
  backendOf,
  baseRecipe,
  buildRecipeCommand,
  buildRecipePreamble,
  recipeDefaults,
  resolveRecipe,
  templatesForBackend,
  BACKENDS,
  SLURM_RECIPES,
  type Backend,
  type BackendId,
  type SlurmRecipe,
} from "@/lib/model-recipes";
import {
  ResourceFields,
  type ResourceValue,
} from "@/components/slurm-launcher/resource-fields";
import { useClusterResources } from "@/components/slurm-launcher/use-cluster-resources";
import { PerformanceFields } from "@/components/slurm-launcher/performance-fields";
import { BackendPicker } from "@/components/slurm-launcher/backend-picker";
import { TemplatePicker } from "@/components/slurm-launcher/template-picker";
import { type LauncherSpec } from "@/components/slurm-launcher/spec";
import { cn } from "@/lib/utils";

export type { LauncherSpec };

export type LauncherSubmit = (
  formData: FormData,
) => Promise<{ ok: boolean; error?: string }>;

type Stage = "backend" | "template" | "configure";
export type SlurmLauncherStage = Stage;

const SLURM_STAGE_META: Record<Stage, { step: string; title: string; description: string }> = {
  backend: {
    step: "Step 2 of 4",
    title: "Choose a serving engine",
    description: "Pick the runtime that should start under Slurm and answer OpenAI-compatible traffic.",
  },
  template: {
    step: "Step 3 of 4",
    title: "Pick a launch template",
    description: "Start from a tuned recipe, reuse one you saved, or open a clean backend form.",
  },
  configure: {
    step: "Step 4 of 4",
    title: "Configure the managed route",
    description: "Set the API id, model handle, Slurm resources, health checks, and launch behavior.",
  },
};

const MODEL_TYPE_OPTIONS = [
  { value: "chat", label: "Chat / completions" },
  { value: "embedding", label: "Embeddings" },
  { value: "audio_transcription", label: "Audio transcription (STT)" },
  { value: "audio_speech", label: "Text to speech (TTS)" },
  { value: "image", label: "Image generation" },
] as const;

const EMPTY_RESOURCES: ResourceValue = {
  partition: "",
  node: "",
  gres: "",
  cpusPerTask: "",
  mem: "",
};

function normalizeApiNameDraft(value: string) {
  return value
    .toLowerCase()
    .replace(/[\s_]+/g, "-")
    .replace(/[^a-z0-9.-]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^\.+/g, "");
}

function normalizeApiNameFinal(value: string) {
  return normalizeApiNameDraft(value).replace(/^[.-]+|[.-]+$/g, "");
}

export function SlurmLauncher(props: {
  mode: "create" | "edit";
  recipes?: readonly SlurmRecipe[];
  onSubmit: LauncherSubmit;
  onCancel?: () => void;
  onBackToHost?: () => void;
  busy?: boolean;
  initialSpec?: LauncherSpec;
  embedded?: boolean;
  stage?: SlurmLauncherStage;
  onStageChange?: (stage: SlurmLauncherStage) => void;
}): React.ReactElement {
  const recipes = props.recipes ?? SLURM_RECIPES;
  const allRecipes = useMemo(
    () => [...recipes, ...SLURM_RECIPES],
    [recipes],
  );
  const controlledStage = props.stage !== undefined;
  const onStageChange = props.onStageChange;
  const [internalStage, setInternalStage] = useState<Stage>(
    props.mode === "edit" ? "configure" : "backend",
  );
  const stage = props.stage ?? internalStage;

  function setStage(next: Stage) {
    if (!controlledStage) setInternalStage(next);
    onStageChange?.(next);
  }

  useEffect(() => {
    if (!controlledStage) onStageChange?.(stage);
  }, [controlledStage, onStageChange, stage]);

  const [backend, setBackend] = useState<BackendId | null>(null);
  const [recipe, setRecipe] = useState<SlurmRecipe>(() => baseRecipe("llamacpp"));
  const isCustom = recipe.manual === true;

  const [modelName, setModelName] = useState("");
  const [modelType, setModelType] = useState("chat");
  const [model, setModel] = useState("");
  const [port, setPort] = useState("8000");
  const [recipeValues, setRecipeValues] = useState<Record<string, string>>({});
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
  const [saveName, setSaveName] = useState("");

  const { data: resourcesData } = useClusterResources();
  const queryClient = useQueryClient();

  function currentSpec(): LauncherSpec {
    return {
      backendId: recipe.id,
      model,
      port,
      recipeValues,
      preamble,
      resources,
      vramGb,
      nodes,
      replicas,
      healthPath,
      maxJobFailures,
      image,
      logOutputDir,
      account,
      qos,
      timeLimit,
      constraints,
      exclude,
      scriptBody,
    };
  }

  function applySpec(spec: LauncherSpec) {
    const r = resolveRecipe(allRecipes, spec.backendId);
    if (r) {
      setRecipe(r);
      setBackend(backendOf(r));
    } else if (spec.backendId) {
      const b = spec.backendId as BackendId;
      const base = baseRecipe(b);
      setRecipe(base);
      setBackend(backendOf(base));
    }
    if (spec.model !== undefined) setModel(spec.model);
    if (spec.port !== undefined) setPort(spec.port);
    if (spec.recipeValues !== undefined) setRecipeValues(spec.recipeValues);
    if (spec.preamble !== undefined) setPreamble(spec.preamble);
    if (spec.resources !== undefined)
      setResources({ ...EMPTY_RESOURCES, ...spec.resources });
    if (spec.vramGb !== undefined) setVramGb(spec.vramGb);
    if (spec.nodes !== undefined) setNodes(spec.nodes);
    if (spec.replicas !== undefined) setReplicas(spec.replicas);
    if (spec.healthPath !== undefined) setHealthPath(spec.healthPath);
    if (spec.maxJobFailures !== undefined) setMaxJobFailures(spec.maxJobFailures);
    if (spec.image !== undefined) setImage(spec.image);
    if (spec.logOutputDir !== undefined) setLogOutputDir(spec.logOutputDir);
    if (spec.account !== undefined) setAccount(spec.account);
    if (spec.qos !== undefined) setQos(spec.qos);
    if (spec.timeLimit !== undefined) setTimeLimit(spec.timeLimit);
    if (spec.constraints !== undefined) setConstraints(spec.constraints);
    if (spec.exclude !== undefined) setExclude(spec.exclude);
    if (spec.scriptBody !== undefined) setScriptBody(spec.scriptBody);
  }

  const applied = useRef(false);
  useEffect(() => {
    if (!applied.current && props.initialSpec) {
      applied.current = true;
      applySpec(props.initialSpec);
      setStage("configure");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function loadRecipe(next: SlurmRecipe) {
    setRecipe(next);
    setBackend(backendOf(next));
    setRecipeValues(recipeDefaults(next));
    setHealthPath(next.healthPath);
    setError(null);
    setStage("configure");
  }

  function pickBackend(b: Backend) {
    setBackend(b.id);
    setError(null);
    if (b.manual) {
      loadRecipe(baseRecipe("custom"));
    } else {
      setStage("template");
    }
  }

  function goBack() {
    setError(null);
    if (stage === "configure") {
      setStage(isCustom ? "backend" : "template");
    } else if (stage === "template") {
      setStage("backend");
    } else {
      props.onBackToHost?.();
    }
  }

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
  const previewBody = isCustom ? scriptBody : generatedCmd;
  const previewScript = [effectivePreamble, previewBody].filter(Boolean).join("\n\n");

  function setRecipeValue(id: string, value: string) {
    setRecipeValues((current) => ({ ...current, [id]: value }));
  }

  function updateModelName(value: string, finalize = false) {
    const next = finalize ? normalizeApiNameFinal(value) : normalizeApiNameDraft(value);
    setModelName(next);
    if (error) setError(null);
  }

  const saveMutation = useMutation({
    mutationFn: (name: string) =>
      fetch("/api/live/recipes", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name,
          backend: backendOf(recipe),
          author: "",
          spec: currentSpec(),
        }),
      }).then((r) => {
        if (!r.ok) throw new Error("save failed");
        return r.json();
      }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["recipes"] });
      setSaveName("");
    },
  });

  async function launch() {
    setError(null);
    const apiName = normalizeApiNameFinal(modelName);
    if (props.mode === "create" && !apiName) {
      return setError("API model name is required.");
    }
    if (!resources.partition.trim()) return setError("Partition is required.");
    if (isCustom ? !scriptBody.trim() : !model.trim()) {
      return setError(
        isCustom ? "Enter the job script." : "Enter the model path or id.",
      );
    }

    if (props.mode === "create" && apiName !== modelName) setModelName(apiName);

    const fd = new FormData();
    fd.set("model_name", apiName);
    fd.set("model_type", modelType);
    fd.set("upstream_model", apiName);
    fd.set(
      "context_window",
      recipe.params?.some((p) => p.id === "ctx_size")
        ? recipeValues.ctx_size ?? ""
        : "",
    );

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
    fd.set("slurm_launcher_spec", JSON.stringify(currentSpec()));

    const res = await props.onSubmit(fd);
    if (!res.ok) setError(res.error ?? "Launch failed.");
  }

  const backendMeta = BACKENDS.find((b) => b.id === backend);
  const modelLabel = recipe.modelLabel ?? "Model handle";
  const meta = SLURM_STAGE_META[stage];
  const routeType = MODEL_TYPE_OPTIONS.find((item) => item.value === modelType)?.label ?? modelType;
  const title = props.mode === "edit" ? "Edit managed model" : "Launch a model on Slurm";
  const breadcrumb = [backendMeta?.label, stage === "configure" ? recipe.label : null].filter(Boolean).join(" / ");

  const content = (
    <>
      <div className="grid min-h-[30rem] lg:grid-cols-[minmax(0,1fr)_19rem]">
        <div className="min-w-0 p-5 sm:p-6">
          <div className="mb-5">
            <p className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
              {props.mode === "edit" ? "Managed Slurm" : meta.step}
            </p>
            <h3 className="mt-1 text-lg font-semibold tracking-tight">{meta.title}</h3>
            <p className="mt-1 max-w-2xl text-sm text-muted-foreground">{meta.description}</p>
          </div>

          {error && (
            <p className="mb-4 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
              {error}
            </p>
          )}

          {stage === "backend" && (
            <div className="space-y-4">
              <BackendPicker selected={backend} onPick={pickBackend} />
              <p className="max-w-2xl text-xs leading-relaxed text-muted-foreground">
                You can switch engines before choosing a template. The route itself stays OpenAI-compatible; this choice only controls how Slurm starts the upstream server.
              </p>
            </div>
          )}

          {stage === "template" && backend && (
            <TemplatePicker
              backend={backend}
              templates={templatesForBackend(recipes, backend)}
              allRecipes={allRecipes}
              onUseTemplate={(r) => loadRecipe(r)}
              onUseSaved={(spec) => {
                applySpec(spec);
                setStage("configure");
              }}
              onScratch={() => loadRecipe(baseRecipe(backend))}
            />
          )}

          {stage === "configure" && (
            <div className="space-y-6">
              {props.mode === "create" && (
                <LauncherSection
                  title="Route identity"
                  description="These values become the public API route clients call through obleth."
                >
                  <div className="grid gap-3 sm:grid-cols-2">
                    <div className="space-y-1.5">
                      <Label htmlFor="sl-model-name">API model name</Label>
                      <Input
                        id="sl-model-name"
                        value={modelName}
                        onChange={(e) => updateModelName(e.target.value)}
                        onBlur={() => updateModelName(modelName, true)}
                        placeholder="qwen3-vl-32b-instruct"
                        className="h-9 font-mono text-xs lowercase"
                        autoCapitalize="none"
                        autoCorrect="off"
                        spellCheck={false}
                      />
                      <p className="text-xs text-muted-foreground">
                        Lowercase only; spaces and underscores become dashes.
                      </p>
                    </div>
                    <div className="space-y-1.5">
                      <Label htmlFor="sl-model-type">Route type</Label>
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
                </LauncherSection>
              )}

              <LauncherSection
                title="Slurm placement"
                description="Choose where replicas should land. Discovery fills known partitions and node defaults when available."
              >
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
                      Used only for tuning recommendations when the recipe supports them.
                    </p>
                  </div>
                </div>
              </LauncherSection>

              <LauncherSection
                title={isCustom ? "Job script" : "Runtime"}
                description={isCustom ? "Paste the full script Slurm should run." : "Point the selected engine at a model and container image."}
              >
                {isCustom ? (
                  <div className="space-y-1.5">
                    <Label htmlFor="sl-script">Job script</Label>
                    <textarea
                      id="sl-script"
                      value={scriptBody}
                      onChange={(e) => setScriptBody(e.target.value)}
                      rows={12}
                      spellCheck={false}
                      className="flex w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                      placeholder={"#!/bin/bash\nsrun ..."}
                    />
                  </div>
                ) : (
                  <>
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
                          <p className="text-xs text-muted-foreground">
                            {recipe.modelHint}
                          </p>
                        )}
                      </div>
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
                          <p className="text-xs text-muted-foreground">
                            {recipe.imageHint}
                          </p>
                        )}
                      </div>
                    </div>
                    <PerformanceFields
                      recipe={recipe}
                      values={recipeValues}
                      onChange={setRecipeValue}
                      vramGb={vramGb ? Number(vramGb) : null}
                    />
                    <div className="space-y-1.5">
                      <Label htmlFor="sl-preamble">Extra preamble</Label>
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
                  </>
                )}
              </LauncherSection>

              <LauncherSection
                title="Service behavior"
                description="These are obleth-owned controls around the launched server."
              >
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
              </LauncherSection>

              <div className="space-y-3 border-t border-border/70 pt-5">
                <button
                  type="button"
                  onClick={() => setShowAdvancedPlacement((v) => !v)}
                  className="text-xs font-medium text-muted-foreground transition-colors hover:text-foreground"
                >
                  {showAdvancedPlacement ? "Hide" : "Show"} advanced placement
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

              <div className="space-y-2 rounded-md border border-dashed border-border/80 bg-background/25 p-3">
                <div>
                  <Label htmlFor="sl-save">Save as reusable recipe</Label>
                  <p className="mt-0.5 text-xs text-muted-foreground">
                    Store this launch plan for future models using the same backend.
                  </p>
                </div>
                <div className="flex flex-col gap-2 sm:flex-row">
                  <Input
                    id="sl-save"
                    value={saveName}
                    onChange={(e) => setSaveName(e.target.value)}
                    placeholder="recipe name"
                    className="h-8 text-xs"
                  />
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    className="h-8 shrink-0 text-xs"
                    disabled={!saveName.trim() || saveMutation.isPending}
                    onClick={() => saveMutation.mutate(saveName.trim())}
                  >
                    {saveMutation.isPending ? "Saving..." : "Save recipe"}
                  </Button>
                </div>
                {saveMutation.isError && (
                  <p className="text-xs text-destructive">Could not save recipe.</p>
                )}
                {saveMutation.isSuccess && (
                  <p className="text-xs text-emerald-500">Recipe saved.</p>
                )}
              </div>
            </div>
          )}
        </div>

        <SlurmPreview
          modelName={props.mode === "create" ? modelName : "Saved route"}
          modelType={routeType}
          backendLabel={backendMeta?.label ?? "Not selected"}
          recipeLabel={stage === "configure" ? recipe.label : "Not selected"}
          partition={resources.partition}
          node={resources.node}
          replicas={replicas}
          port={port}
          healthPath={healthPath || recipe.healthPath || "/health"}
          script={previewScript}
          stage={stage}
        />
      </div>

      <div className="flex flex-col gap-3 border-t border-border/70 bg-background/30 px-5 py-4 sm:flex-row sm:items-center sm:justify-between">
        <Button
          type="button"
          variant="ghost"
          onClick={goBack}
          disabled={props.busy}
          className={props.mode === "edit" ? "invisible" : undefined}
        >
          Back
        </Button>
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
          {stage === "configure" && (
            <Button type="button" onClick={launch} disabled={props.busy}>
              {props.mode === "edit"
                ? props.busy
                  ? "Saving..."
                  : "Save changes"
                : props.busy
                  ? "Creating..."
                  : "Create managed model"}
            </Button>
          )}
        </div>
      </div>
    </>
  );

  if (props.embedded) return <div>{content}</div>;

  return (
    <Card className="overflow-hidden">
      <CardHeader className="border-b border-border/70 bg-background/30">
        <CardTitle>{title}</CardTitle>
        <CardDescription>
          {breadcrumb || "Manage the Slurm job recipe, resources, and health checks for this route."}
        </CardDescription>
      </CardHeader>
      <CardContent className="p-0">{content}</CardContent>
    </Card>
  );
}

function LauncherSection({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: React.ReactNode;
}) {
  return (
    <section className="space-y-3 border-t border-border/70 pt-5 first:border-t-0 first:pt-0">
      <div>
        <h4 className="text-sm font-medium">{title}</h4>
        <p className="mt-0.5 text-xs leading-relaxed text-muted-foreground">{description}</p>
      </div>
      <div className="space-y-3">{children}</div>
    </section>
  );
}

function SlurmPreview({
  modelName,
  modelType,
  backendLabel,
  recipeLabel,
  partition,
  node,
  replicas,
  port,
  healthPath,
  script,
  stage,
}: {
  modelName: string;
  modelType: string;
  backendLabel: string;
  recipeLabel: string;
  partition: string;
  node: string;
  replicas: string;
  port: string;
  healthPath: string;
  script: string;
  stage: Stage;
}) {
  return (
    <aside className="border-t border-border/70 bg-background/25 p-5 lg:border-l lg:border-t-0">
      <p className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">Slurm preview</p>
      <div className="mt-3 space-y-3">
        <PreviewRow label="API model name" value={modelName || "Not set"} muted={!modelName} />
        <PreviewRow label="Route type" value={modelType} />
        <PreviewRow label="Engine" value={backendLabel} muted={backendLabel === "Not selected"} />
        <PreviewRow label="Template" value={recipeLabel} muted={recipeLabel === "Not selected"} />
        <PreviewRow label="Partition" value={partition || "Not set"} muted={!partition} />
        {node && <PreviewRow label="Node" value={node} />}
        <PreviewRow label="Replicas" value={replicas || "2"} />
        <PreviewRow label="Service" value={`:${port || "8000"} ${healthPath || "/health"}`} />
      </div>

      <div className="mt-5 rounded-md border border-border/70 bg-card/50 p-3">
        <p className="text-xs font-medium">What happens on create</p>
        <p className="mt-1 text-[11px] leading-relaxed text-muted-foreground">
          obleth creates the route without a static API base. The provisioner starts Slurm replicas, checks health, and adds healthy endpoints to rotation.
        </p>
      </div>

      {stage === "configure" && (
        <div className="mt-5 space-y-2">
          <div className="flex items-center justify-between gap-2">
            <p className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">Launch script</p>
            <Badge className="bg-background text-[10px]">live</Badge>
          </div>
          <pre
            className={cn(
              "max-h-56 overflow-auto whitespace-pre-wrap rounded-md border border-border bg-muted/35 px-3 py-2 font-mono text-[11px] leading-relaxed text-muted-foreground",
              !script && "text-muted-foreground/70",
            )}
          >
            {script || "(nothing yet)"}
          </pre>
        </div>
      )}
    </aside>
  );
}

function PreviewRow({ label, value, muted }: { label: string; value: string; muted?: boolean }) {
  return (
    <div>
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      <p className={cn("mt-0.5 break-words text-sm font-medium", muted && "text-muted-foreground")}>{value}</p>
    </div>
  );
}
