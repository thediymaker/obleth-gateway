"use client";

import { createContext, useActionState, useContext, useEffect, useMemo, useRef, useState, useTransition, type ChangeEvent, type ReactNode } from "react";
import {
  Activity,
  Check,
  ChevronDown,
  Database,
  Download,
  HeartPulse,
  Info,
  MoreHorizontal,
  PauseCircle,
  Plus,
  RefreshCw,
  Save,
  Sparkles,
  Tag,
  Trash2,
  Upload,
  XCircle,
  Zap,
} from "lucide-react";
import {
  CartesianGrid,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import {
  applyAutotuneCapacityAction,
  autotuneModelAction,
  checkModelHealthAction,
  createModelAction,
  createModelEndpointAction,
  deleteModelAction,
  deleteModelEndpointAction,
  importModelsAction,
  planModelImportAction,
  setModelCacheAction,
  setModelCapacityAction,
  setModelCapacityModeAction,
  setModelHealthConfigAction,
  setModelReliabilityAction,
  setModelWeightAction,
  updateModelConnectionAction,
  updateModelCapabilitiesAction,
  updateModelEndpointAction,
  type ImportModelsResult,
  type ImportPlanItem,
  type ModelActionState,
} from "@/app/actions";
import { ChartShell, axisTick, chartGrid, compactAxis, tip, timeCursor } from "@/components/chart-tooltip";
import { ModelMetricsDetail } from "@/components/model-metrics-detail";
import { ManagedModelConfig } from "@/components/managed-model-config";
import { ReplicaPanel } from "@/components/replica-panel";
import { ProvisionErrorBanner } from "@/components/provision-error-banner";
import { Badge } from "@/components/ui/badge";
import { Button, buttonVariants } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Tooltip as UiTooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import type { AutotuneReport, AutotuneWorkload, CacheStats, McpServer, ModelEndpoint, ModelHealthDetail, ModelHealthSummary, ModelRoute } from "@/lib/obleth";
import { providerForModel } from "@/lib/model-providers";
import { RecipeList } from "@/components/recipes/recipe-list";
import type { RecipeCard } from "@/components/recipes/recipe-card";
import { cn, formatNumber } from "@/lib/utils";
import { useSearchParams } from "next/navigation";

// Lets any control inside a model's detail panel signal a successful save so the
// surrounding card can flash its border glow. Default no-op for controls rendered
// outside a panel (e.g. embedded weight controls elsewhere).
const SaveFlashContext = createContext<() => void>(() => {});

// Fixed routing-tag vocabulary; mirrors obleth-config `MODEL_TAGS`. Used by the
// `auto` router to match requests to models.
const MODEL_TAGS = [
  "coding",
  "general",
  "reasoning",
  "math",
  "vision",
  "long-context",
  "fast",
  "creative",
] as const;

// Fixed boon vocabulary; mirrors obleth-config `MODEL_BOONS`. A boon grants a
// capability the model lacks natively. Each boon is configured globally in
// Settings → Boons, then enabled per model here. Nothing is granted by default.
const MODEL_BOONS = [
  {
    value: "vision",
    label: "Vision",
    description:
      "Relay image inputs to the global describer model and inject text descriptions, so this model can accept images it doesn't natively support. Configure the describer in Settings → Boons.",
  },
  {
    value: "structured_output",
    label: "Structured output",
    description:
      "Enforce response_format JSON schemas at the gateway: the schema is rendered into the prompt and the reply is validated, with invalid JSON repaired by the configured fixer model. Applies only when the model lacks the Response schema capability. Configure in Settings → Boons.",
  },
] as const;

// Model modality vocabulary; mirrors obleth-config `MODEL_TYPES`. The type
// determines which OpenAI endpoint the route serves and how it is billed.
const MODEL_TYPE_OPTIONS = [
  { value: "chat", label: "Chat / completions" },
  { value: "embedding", label: "Embeddings" },
  { value: "audio_transcription", label: "Audio transcription (STT)" },
  { value: "audio_speech", label: "Text to speech (TTS)" },
  { value: "image", label: "Image generation" },
] as const;

const MODEL_TYPE_LABELS: Record<string, string> = Object.fromEntries(
  MODEL_TYPE_OPTIONS.map((o) => [o.value, o.label]),
);

const CREATE_MODEL_STEPS = [
  {
    label: "Hosting",
    title: "Choose a hosting option",
    description: "Select the backend that will serve this route. More hosting options can be added here later.",
  },
  {
    label: "API ID",
    title: "Choose the API model name",
    description: "This is the exact model value clients pass to the API, not a friendly display name.",
  },
  {
    label: "Connection",
    title: "Provide connection details",
    description: "Point obleth at the upstream endpoint or define the Slurm launch details.",
  },
  {
    label: "Traffic",
    title: "Set traffic rules",
    description: "Start with sensible capacity and billing defaults; tune them later as load changes.",
  },
  {
    label: "Review",
    title: "Capabilities and review",
    description: "Attach chat capabilities, routing tags, boons, and tool grants before creating the route.",
  },
] as const;


function normalizeModelApiNameDraft(value: string) {
  return value
    .toLowerCase()
    .replace(/[\s_]+/g, "-")
    .replace(/[^a-z0-9.-]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^\.+/g, "");
}

function normalizeModelApiNameFinal(value: string) {
  return normalizeModelApiNameDraft(value).replace(/^[.-]+|[.-]+$/g, "");
}

export function ModelManager({
  models,
  cacheStats,
  health,
  healthDetails,
  endpoints,
  mcpServers = [],
  managed = {},
  slurmEnabled = false,
  recipeCards = [],
}: {
  models: ModelRoute[];
  cacheStats?: CacheStats;
  health: ModelHealthSummary[];
  healthDetails: Record<string, ModelHealthDetail | undefined>;
  endpoints: Record<string, ModelEndpoint[]>;
  mcpServers?: McpServer[];
  managed?: Record<string, boolean>;
  slurmEnabled?: boolean;
  recipeCards?: RecipeCard[];
}) {
  const [pending, start] = useTransition();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  // Deep-link support: `/models?model=<model_name>` (used by the overview
  // "Open in Models" link) auto-expands the matching route on first load.
  const searchParams = useSearchParams();
  const requestedModel = searchParams.get("model");
  useEffect(() => {
    if (!requestedModel) return;
    const match = models.find((m) => m.model_name === requestedModel);
    if (match) setSelectedId(match.id);
  }, [requestedModel, models]);
  // Bumps a counter scoped to a model id so its card replays the save-glow on
  // every successful save (the changing `n` remounts the overlay to restart CSS).
  const [saveFlash, setSaveFlash] = useState<{ id: string; n: number } | null>(null);
  const flashSaved = (id: string) =>
    setSaveFlash((prev) => ({ id, n: prev?.id === id ? prev.n + 1 : 1 }));
  const [createWizardOpen, setCreateWizardOpen] = useState(false);
  const [showBenchmarkRoutes, setShowBenchmarkRoutes] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const importInputRef = useRef<HTMLInputElement>(null);
  const [importResult, setImportResult] = useState<ImportModelsResult | null>(null);
  const [importPlan, setImportPlan] = useState<ImportPlanItem[] | null>(null);
  const [importText, setImportText] = useState<string>("");
  const [importError, setImportError] = useState<string | null>(null);
  const healthByModel = useMemo(() => new Map(health.map((row) => [row.model_id, row])), [health]);
  const benchmarkRouteCount = models.filter(isBenchmarkRoute).length;
  const visibleModels = showBenchmarkRoutes ? models : models.filter((model) => !isBenchmarkRoute(model));

  function removeModel(model: ModelRoute) {
    if (!window.confirm(`Remove model route "${model.model_name}"? This cannot be undone.`)) return;
    start(() => deleteModelAction(model.id));
  }

  function exportModels() {
    const payload = {
      version: 1 as const,
      exported_at: new Date().toISOString(),
      models: models.map(toExportShape),
    };
    const blob = new Blob([JSON.stringify(payload, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = `obleth-models-${new Date().toISOString().slice(0, 10)}.json`;
    document.body.appendChild(link);
    link.click();
    link.remove();
    URL.revokeObjectURL(url);
  }

  function onImportFile(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;
    setImportResult(null);
    setImportPlan(null);
    setImportError(null);
    const reader = new FileReader();
    reader.onload = () => {
      const text = String(reader.result ?? "");
      start(async () => {
        const plan = await planModelImportAction(text);
        if (plan.ok) {
          setImportText(text);
          setImportPlan(plan.plan);
        } else {
          setImportError(plan.error);
        }
      });
    };
    reader.onerror = () => setImportError("Could not read the selected file.");
    reader.readAsText(file);
  }

  function confirmImport() {
    if (!importText) return;
    start(async () => {
      const result = await importModelsAction(importText);
      setImportResult(result);
      setImportPlan(null);
      setImportText("");
    });
  }

  function cancelImport() {
    setImportPlan(null);
    setImportText("");
    setImportError(null);
  }

  function submitModel(formData: FormData) {
    setCreateError(null);
    start(async () => {
      const result = await createModelAction(formData);
      if (result.ok) {
        setCreateWizardOpen(false);
      } else {
        setCreateError(result.error);
      }
    });
  }

  function openCreateWizard() {
    setCreateError(null);
    setCreateWizardOpen(true);
  }

  function closeCreateWizard() {
    setCreateError(null);
    setCreateWizardOpen(false);
  }

  function checkAll() {
    start(async () => {
      for (const model of visibleModels) {
        await checkModelHealthAction(model.id);
      }
    });
  }

  function checkOne(id: string) {
    start(() => checkModelHealthAction(id));
  }

  return (
    <div className="space-y-6">
      <FleetStats models={models} health={health} cacheStats={cacheStats} />
      {createWizardOpen && (
        <CreateModelWizard
          pending={pending}
          error={createError}
          slurmEnabled={slurmEnabled}
          mcpServers={mcpServers}
          recipeCards={recipeCards}
          onCancel={closeCreateWizard}
          onSubmit={submitModel}
        />
      )}

      <Card>
        <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div>
            <CardTitle>Configured models</CardTitle>
            <CardDescription>
              {visibleModels.length} visible / {models.length} registered
              {!showBenchmarkRoutes && benchmarkRouteCount > 0 ? ` / ${benchmarkRouteCount} benchmark hidden` : ""}
            </CardDescription>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            {benchmarkRouteCount > 0 && (
              <Button type="button" size="sm" variant="outline" onClick={() => setShowBenchmarkRoutes((value) => !value)}>
                {showBenchmarkRoutes ? "Hide benchmark" : "Show benchmark"}
              </Button>
            )}
            <input
              ref={importInputRef}
              type="file"
              accept=".yaml,.yml,.json,text/yaml,application/json"
              className="hidden"
              onChange={onImportFile}
            />
            <Button type="button" size="sm" variant="outline" disabled={pending} onClick={() => importInputRef.current?.click()}>
              <Upload className="h-3.5 w-3.5" />
              Import
            </Button>
            <Button type="button" size="sm" variant="outline" disabled={models.length === 0} onClick={exportModels}>
              <Download className="h-3.5 w-3.5" />
              Export
            </Button>
            <Button type="button" size="sm" variant="secondary" disabled={pending || visibleModels.length === 0} onClick={checkAll}>
              <RefreshCw className="h-3.5 w-3.5" />
              Check listed
            </Button>
            <Button
              type="button"
              size="sm"
              variant={createWizardOpen ? "secondary" : "default"}
              onClick={createWizardOpen ? closeCreateWizard : openCreateWizard}
            >
              <Plus className="h-3.5 w-3.5" />
              {createWizardOpen ? "Close setup" : "New model"}
            </Button>
          </div>
        </CardHeader>
        <CardContent className="p-0">
          {importError && (
            <div className="px-6 pt-4">
              <div className="flex items-start justify-between gap-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
                <span>Import failed: {importError}</span>
                <button type="button" onClick={() => setImportError(null)} className="shrink-0 text-xs underline opacity-80 hover:opacity-100">
                  Dismiss
                </button>
              </div>
            </div>
          )}
          {importPlan && (
            <div className="px-6 pt-4">
              <ImportPreview plan={importPlan} pending={pending} onConfirm={confirmImport} onCancel={cancelImport} />
            </div>
          )}
          {importResult && (
            <div className="px-6 pt-4">
              <ImportResultBanner result={importResult} onDismiss={() => setImportResult(null)} />
            </div>
          )}
          <div className="text-sm">
            <div className="grid border-b border-border text-left text-xs text-muted-foreground md:grid-cols-[40fr_16fr_32fr_12fr]">
              <div className="px-6 py-3 font-medium">Model</div>
              <div className="hidden px-3 py-3 font-medium md:block">Health</div>
              <div className="hidden px-3 py-3 font-medium md:block">Route</div>
              <div className="hidden px-3 py-3 text-right font-medium md:block" />
            </div>
            <div className="space-y-3 px-4 py-4">
              {visibleModels.map((model) => {
                const summary = healthByModel.get(model.id) ?? fallbackHealth(model);
                const selected = selectedId === model.id;
                return (
                  <div
                    key={model.id}
                    className={cn(
                      "group relative overflow-hidden rounded-lg border shadow-sm transition-colors",
                      selected
                        ? "border-primary/35 bg-muted/25 ring-1 ring-primary/15"
                        : "border-border/70 bg-card/35 hover:border-border hover:bg-muted/15",
                    )}
                  >
                    {saveFlash?.id === model.id && (
                      <span
                        key={saveFlash.n}
                        aria-hidden
                        className="card-saved-glow pointer-events-none absolute inset-0 z-30 rounded-lg"
                      />
                    )}
                    <div
                      onClick={() => setSelectedId((current) => (current === model.id ? null : model.id))}
                      className="relative cursor-pointer overflow-hidden transition-colors"
                    >
                      <ModelProviderBackdrop name={model.model_name} upstream={model.upstream_model} selected={selected} />
                      <div className="relative z-10 grid min-w-0 md:grid-cols-[40fr_16fr_32fr_12fr] md:items-center">
                        <div className="min-w-0 px-5 py-3.5 pr-14 md:pr-5">
                          <div className="min-w-0">
                            <p className="truncate font-medium" title={model.model_name}>
                              {model.model_name}
                            </p>
                            {model.description && (
                              <p className="mt-0.5 line-clamp-2 text-xs leading-snug text-muted-foreground" title={model.description}>
                                {model.description}
                              </p>
                            )}
                            <div className="mt-1.5 flex flex-wrap items-center gap-x-2.5 gap-y-1">
                              {model.model_type && model.model_type !== "chat" && (
                                <Badge className="border-primary/40 bg-primary/15 text-[10px] text-primary">
                                  {MODEL_TYPE_LABELS[model.model_type] ?? model.model_type}
                                </Badge>
                              )}
                              <Badge className="border-border bg-background text-[10px] text-muted-foreground">{formatModelCost(model)}</Badge>
                              <Badge className="border-border bg-background text-[10px] text-muted-foreground">{formatNumber(model.context_window)} ctx</Badge>
                              {(model.tags?.length ?? 0) > 0 && (
                                <span className="inline-flex items-center gap-1 text-[11px] text-muted-foreground">
                                  <Tag className="h-3 w-3" aria-hidden />
                                  {model.tags!.join(" · ")}
                                </span>
                              )}
                              {(model.boons?.length ?? 0) > 0 && (
                                <span className="inline-flex items-center gap-1 text-[11px] text-amber-600 dark:text-amber-400/90">
                                  <Sparkles className="h-3 w-3" aria-hidden />
                                  {model.boons!.join(" · ")}
                                </span>
                              )}
                            </div>
                            <p className="mt-1 truncate font-mono text-[11px] text-muted-foreground md:hidden" title={model.upstream_model}>
                              {model.upstream_model}
                            </p>
                            {model.api_base && (
                              <p className="mt-0.5 truncate font-mono text-[10px] text-muted-foreground/70 md:hidden" title={model.api_base}>
                                {middleTruncate(model.api_base, 56)}
                              </p>
                            )}
                          </div>
                        </div>
                        <div className="hidden min-w-0 px-3 py-3.5 md:block">
                          <HealthCell summary={summary} />
                        </div>
                        <div className="hidden min-w-0 px-3 py-3.5 md:block">
                          <p className="truncate font-mono text-xs text-muted-foreground" title={model.upstream_model}>
                            {model.upstream_model}
                          </p>
                          <p className="mt-0.5 truncate font-mono text-[11px] text-muted-foreground/70" title={model.api_base}>
                            {middleTruncate(model.api_base, 56)}
                          </p>
                        </div>
                        <div className="absolute right-3 top-3 md:static md:px-3 md:py-3.5">
                          <div className="flex items-center justify-end gap-1">
                            <DropdownMenu>
                              <DropdownMenuTrigger asChild>
                                <Button
                                  type="button"
                                  size="icon"
                                  variant="ghost"
                                  className="h-8 w-8 text-muted-foreground hover:text-foreground"
                                  disabled={pending}
                                  title="Model actions"
                                  onClick={(event) => event.stopPropagation()}
                                >
                                  <MoreHorizontal className="h-4 w-4" />
                                </Button>
                              </DropdownMenuTrigger>
                              <DropdownMenuContent align="end" onClick={(event) => event.stopPropagation()}>
                                <DropdownMenuItem onSelect={() => checkOne(model.id)}>
                                  <Activity className="mr-2 h-3.5 w-3.5" />
                                  Check health
                                </DropdownMenuItem>
                                <DropdownMenuSeparator />
                                <DropdownMenuItem
                                  className="text-destructive focus:text-destructive"
                                  onSelect={() => removeModel(model)}
                                >
                                  <Trash2 className="mr-2 h-3.5 w-3.5" />
                                  Delete model
                                </DropdownMenuItem>
                              </DropdownMenuContent>
                            </DropdownMenu>
                            <ChevronDown
                              aria-hidden
                              className={cn(
                                "h-3.5 w-3.5 text-muted-foreground transition-transform duration-200",
                                selected && "rotate-180 text-foreground",
                              )}
                            />
                          </div>
                        </div>
                        <div className="border-t border-border/40 px-5 pb-3.5 pt-0 md:hidden">
                          <HealthCell summary={summary} />
                        </div>
                      </div>
                    </div>
                    {selected && (
                      <div className="relative overflow-hidden border-t border-border/60 bg-muted/10">
                        <ModelProviderBackdrop name={model.model_name} upstream={model.upstream_model} selected variant="detail" />
                        <div className="relative z-10 px-5 py-4">
                          <SaveFlashContext.Provider value={() => flashSaved(model.id)}>
                            <ModelDetailPanel
                              model={model}
                              summary={summary}
                              detail={healthDetails[model.id]}
                              endpoints={endpoints[model.id] ?? []}
                              mcpServers={mcpServers}
                              isManaged={managed[model.id] ?? false}
                              pending={pending}
                              onCacheToggle={() => {
                                flashSaved(model.id);
                                start(() => setModelCacheAction(model.id, !model.cache_enabled, model.cache_ttl_secs || 300));
                              }}
                            />
                          </SaveFlashContext.Provider>
                        </div>
                      </div>
                    )}
                  </div>
                );
              })}
              {visibleModels.length === 0 && (
                <div className="rounded-lg border border-dashed border-border/70 px-6 py-10 text-center text-muted-foreground">
                  {models.length === 0 ? "No models configured." : "Only benchmark endpoints are hidden."}
                </div>
              )}
            </div>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function HostingChoice({
  slurmEnabled,
  hostingMode,
  setHostingMode,
}: {
  slurmEnabled: boolean;
  hostingMode: string;
  setHostingMode: (mode: string) => void;
}) {
  return (
    <section className="grid gap-3 lg:grid-cols-2">
      <button
        type="button"
        aria-pressed={hostingMode === "static"}
        onClick={() => setHostingMode("static")}
        className={cn(
          "flex min-h-36 items-start gap-3 rounded-lg border p-4 text-left transition-colors",
          hostingMode === "static"
            ? "border-primary/50 bg-primary/10"
            : "border-border/70 bg-background/35 hover:bg-accent",
        )}
      >
        <Database className="mt-0.5 h-5 w-5 shrink-0 text-muted-foreground" />
        <span>
          <span className="block text-sm font-medium">OpenAI-compatible API</span>
          <span className="mt-1 block text-xs leading-relaxed text-muted-foreground">
            Connect an existing endpoint that speaks the OpenAI API.
          </span>
          <Badge className="mt-3 bg-background text-[10px]">External API</Badge>
        </span>
      </button>
      <button
        type="button"
        aria-pressed={hostingMode === "slurm"}
        disabled={!slurmEnabled}
        onClick={() => setHostingMode("slurm")}
        className={cn(
          "flex min-h-36 items-start gap-3 rounded-lg border p-4 text-left transition-colors disabled:cursor-not-allowed disabled:opacity-50",
          hostingMode === "slurm"
            ? "border-primary/50 bg-primary/10"
            : "border-border/70 bg-background/35 hover:bg-accent",
        )}
      >
        <Zap className="mt-0.5 h-5 w-5 shrink-0 text-muted-foreground" />
        <span>
          <span className="block text-sm font-medium">Managed Slurm</span>
          <span className="mt-1 block text-xs leading-relaxed text-muted-foreground">
            Launch and replace model replicas on your Slurm cluster.
          </span>
          <Badge className="mt-3 bg-background text-[10px]">
            {slurmEnabled ? "Cluster managed" : "Not configured"}
          </Badge>
        </span>
      </button>
      {!slurmEnabled && (
        <p className="rounded-md border border-border/70 bg-background/35 px-3 py-2 text-xs text-muted-foreground lg:col-span-2">
          Configure Slurm in Settings before creating Managed Slurm models.
        </p>
      )}
    </section>
  );
}

function CreateModelWizard({
  pending,
  error,
  slurmEnabled,
  mcpServers,
  recipeCards,
  onCancel,
  onSubmit,
}: {
  pending: boolean;
  error: string | null;
  slurmEnabled: boolean;
  mcpServers: McpServer[];
  recipeCards: RecipeCard[];
  onCancel: () => void;
  onSubmit: (formData: FormData) => void;
}) {
  const [step, setStep] = useState(0);
  const [createType, setCreateType] = useState<string>("chat");
  const [modelName, setModelName] = useState("");
  const [endpointMode, setEndpointMode] = useState<string>("static");
  const [localError, setLocalError] = useState<string | null>(null);
  const [preview, setPreview] = useState({
    modelName: "",
    upstreamModel: "",
    apiBase: "",
  });
  const formRef = useRef<HTMLFormElement>(null);
  const lastStep = CREATE_MODEL_STEPS.length - 1;
  const visibleError = localError ?? error;
  const hostingMode = slurmEnabled ? endpointMode : "static";
  const usingSlurm = hostingMode === "slurm";
  const visibleSteps = CREATE_MODEL_STEPS;
  const visibleStep = step;
  const typeLabel = MODEL_TYPE_OPTIONS.find((option) => option.value === createType)?.label ?? createType;
  const hostingLabel = hostingMode !== "slurm" ? "OpenAI-compatible API" : "Managed Slurm";

  function value(formData: FormData, name: string) {
    return String(formData.get(name) ?? "").trim();
  }

  function formSnapshot() {
    if (!formRef.current) return null;
    return new FormData(formRef.current);
  }

  function updatePreview() {
    const formData = formSnapshot();
    if (!formData) return;
    setPreview({
      modelName: normalizeModelApiNameDraft(value(formData, "model_name")),
      upstreamModel: value(formData, "upstream_model"),
      apiBase: value(formData, "api_base"),
    });
    if (localError) setLocalError(null);
  }

  function updateModelName(value: string, finalize = false) {
    const next = finalize ? normalizeModelApiNameFinal(value) : normalizeModelApiNameDraft(value);
    setModelName(next);
    setPreview((current) => ({ ...current, modelName: next }));
    if (localError) setLocalError(null);
  }

  function setHostingMode(mode: string) {
    setEndpointMode(mode);
    if (localError) setLocalError(null);
  }

  function stepIssue(index: number, formData: FormData) {
    if (index === 1) {
      if (!normalizeModelApiNameFinal(value(formData, "model_name"))) return "Add the API model name clients will use.";
      if (!value(formData, "upstream_model")) return "Add the upstream model identifier.";
    }
    if (index === 2) {
      if (hostingMode === "static" && !value(formData, "api_base")) return "Add the API base URL for the upstream.";
    }
    return null;
  }

  function goToStep(nextStep: number) {
    const boundedStep = Math.max(0, Math.min(lastStep, nextStep));
    if (boundedStep === step) return;
    const formData = formSnapshot();
    if (!formData) return;
    if (boundedStep > step) {
      for (let i = step; i < boundedStep; i += 1) {
        const issue = stepIssue(i, formData);
        if (issue) {
          setLocalError(issue);
          setStep(i);
          return;
        }
      }
    }
    setLocalError(null);
    setStep(boundedStep);
  }

  function goToVisibleStep(nextStep: number) {
    goToStep(nextStep);
  }

  function handleSubmit(formData: FormData) {
    for (let i = 0; i <= lastStep; i += 1) {
      const issue = stepIssue(i, formData);
      if (issue) {
        setLocalError(issue);
        setStep(i);
        return;
      }
    }
    setLocalError(null);
    onSubmit(formData);
  }

  return (
    <Card className="overflow-hidden border-primary/25 bg-card/80">
      <CardHeader className="border-b border-border/70 bg-background/30">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div>
            <CardTitle>New model setup</CardTitle>
            <CardDescription>
              Create a route in a few focused steps, then fine-tune it from the model card.
            </CardDescription>
          </div>
          <Button type="button" size="sm" variant="ghost" disabled={pending} onClick={onCancel}>
            Cancel
          </Button>
        </div>

        {!usingSlurm && (
          <div className="pt-2">
            <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-5">
              {visibleSteps.map((item, index) => {
                const complete = index < visibleStep;
                const active = index === visibleStep;
                const canClick = true;
                return (
                  <button
                    key={item.label}
                    type="button"
                    disabled={pending || !canClick}
                    onClick={() => goToVisibleStep(index)}
                    className={cn(
                      "flex min-w-0 items-center gap-2 rounded-md border px-3 py-2 text-left transition-colors disabled:cursor-not-allowed",
                      active
                        ? "border-primary/45 bg-primary/10 text-foreground"
                        : complete
                          ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-200"
                          : "border-border/70 bg-background/40 text-muted-foreground hover:bg-accent hover:text-foreground",
                      !canClick && !active && "opacity-70",
                    )}
                  >
                    <span
                      className={cn(
                        "flex h-6 w-6 shrink-0 items-center justify-center rounded-full border text-[11px] font-semibold tabular-nums",
                        active
                          ? "border-primary/60 bg-primary text-primary-foreground"
                          : complete
                            ? "border-emerald-500/40 bg-emerald-500/20 text-emerald-200"
                            : "border-border bg-card",
                      )}
                    >
                      {complete ? <Check className="h-3.5 w-3.5" strokeWidth={2.5} /> : index + 1}
                    </span>
                    <span className="min-w-0">
                      <span className="block truncate text-xs font-medium">{item.label}</span>
                      <span className="block truncate text-[10px] text-muted-foreground">{item.title}</span>
                    </span>
                  </button>
                );
              })}
            </div>
            <div className="mt-3 h-1 overflow-hidden rounded-full bg-muted">
              <div
                className="h-full rounded-full bg-primary transition-[width] duration-200"
                style={{ width: `${((visibleStep + 1) / visibleSteps.length) * 100}%` }}
              />
            </div>
          </div>
        )}
      </CardHeader>

      <CardContent className="p-0">
        {usingSlurm ? (
          <div className="space-y-5 p-5 sm:p-6">
            <HostingChoice
              slurmEnabled={slurmEnabled}
              hostingMode={hostingMode}
              setHostingMode={setHostingMode}
            />
            <div>
              <h3 className="text-lg font-semibold tracking-tight">Deploy a recipe</h3>
              <p className="mt-1 max-w-2xl text-sm text-muted-foreground">
                Pick an admin-authored <code className="font-mono">*.recipe</code> file. Edit the file on disk
                and refresh to change it — no restart needed.
              </p>
              <div className="mt-4">
                <RecipeList recipes={recipeCards} onDeployed={onCancel} />
              </div>
            </div>
          </div>
        ) : (
        <form ref={formRef} action={handleSubmit} onInput={updatePreview} onChange={updatePreview}>
          <input type="hidden" name="endpoint_mode" value={hostingMode} />
          <div className="grid min-h-[30rem] lg:grid-cols-[minmax(0,1fr)_18rem]">
            <div className="min-w-0 p-5 sm:p-6">
              <div className="mb-5">
                <p className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
                  Step {visibleStep + 1} of {visibleSteps.length}
                </p>
                <h3 className="mt-1 text-lg font-semibold tracking-tight">{visibleSteps[visibleStep].title}</h3>
                <p className="mt-1 max-w-2xl text-sm text-muted-foreground">
                  {visibleSteps[visibleStep].description}
                </p>
              </div>

              {visibleError && (
                <p className="mb-4 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
                  {visibleError}
                </p>
              )}

              <div className={cn(step !== 0 && "hidden")}>
                <HostingChoice
                  slurmEnabled={slurmEnabled}
                  hostingMode={hostingMode}
                  setHostingMode={setHostingMode}
                />
              </div>

              <section className={cn("grid gap-4 md:grid-cols-2", step !== 1 && "hidden")}>
                <div className="space-y-1.5">
                  <Label htmlFor="model-api-name">API model name</Label>
                  <Input
                    id="model-api-name"
                    name="model_name"
                    value={modelName}
                    onChange={(event) => updateModelName(event.target.value)}
                    onBlur={() => updateModelName(modelName, true)}
                    placeholder="qwen3-vl-32b-instruct"
                    autoCapitalize="none"
                    autoCorrect="off"
                    spellCheck={false}
                    className="font-mono lowercase"
                  />
                  <p className="text-xs text-muted-foreground">
                    Not a display name. API clients pass this exact value as <code className="font-mono text-foreground">model</code>. Lowercase only; spaces and underscores become dashes.
                  </p>
                </div>
                <Field label="Upstream/native model" name="upstream_model" placeholder="Qwen/Qwen3-8B" />
                <div className="md:col-span-2">
                  <SelectField
                    label="Route type"
                    name="model_type"
                    value={createType}
                    onChange={setCreateType}
                    options={MODEL_TYPE_OPTIONS}
                    hint={modelTypeHint(createType)}
                  />
                </div>
                <div className="md:col-span-2">
                  <Field label="Description (optional)" name="description" placeholder="Qwen3 235B instruction model for production chat and tool use" />
                </div>
              </section>

              <section className={cn("grid gap-4 md:grid-cols-2", step !== 2 && "hidden")}>
                <div className="md:col-span-2">
                  <Field label="API base URL" name="api_base" placeholder="http://envoy-aibrix-system.../v1" />
                </div>
                <Field label="Upstream API key (optional)" name="api_key" placeholder="sk_..." />
              </section>

              <section className={cn("grid gap-4 md:grid-cols-2", step !== 3 && "hidden")}>
                <Field label="Admission weight" name="admission_weight" type="number" defaultValue="100" />
                <Field label="Max in-flight (optional)" name="max_in_flight" type="number" placeholder="No cap" />
                {(createType === "chat" || createType === "embedding") && (
                  <Field label="Input cost / token" name="input_cost_per_token" placeholder="0.000000071" />
                )}
                {createType === "chat" && (
                  <Field label="Output cost / token" name="output_cost_per_token" placeholder="0.0000001" />
                )}
                {createType === "image" && (
                  <Field label="Cost / image" name="cost_per_image" placeholder="0.04" />
                )}
                {createType === "audio_speech" && (
                  <Field label="Cost / character" name="cost_per_character" placeholder="0.000015" />
                )}
                {createType === "audio_transcription" && (
                  <Field label="Cost / audio second" name="cost_per_audio_second" placeholder="0.0001" />
                )}
                {(createType === "chat" || createType === "embedding") && (
                  <Field label="Context window" name="context_window" type="number" defaultValue="131072" />
                )}
              </section>

              <section className={cn("space-y-3", step !== 4 && "hidden")}>
                {createType === "chat" ? (
                  <ChatCapabilityFields mcpServers={mcpServers} />
                ) : (
                  <div className="rounded-md border border-border/70 bg-background/35 p-4">
                    <p className="text-sm font-medium">No chat-only capability flags for this route type.</p>
                    <p className="mt-1 text-xs text-muted-foreground">
                      Specialized routes keep their setup focused on hosting, capacity, and billing.
                    </p>
                  </div>
                )}
              </section>
            </div>

            <aside className="border-t border-border/70 bg-background/25 p-5 lg:border-l lg:border-t-0">
              <p className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">Route preview</p>
              <div className="mt-3 space-y-3">
                <PreviewRow label="API model name" value={preview.modelName || "Not set"} muted={!preview.modelName} />
                <PreviewRow label="Upstream" value={preview.upstreamModel || "Not set"} muted={!preview.upstreamModel} />
                <PreviewRow label="Type" value={typeLabel} />
                <PreviewRow label="Hosting" value={hostingLabel} />
                <PreviewRow label="API base" value={preview.apiBase || "Not set"} muted={!preview.apiBase} />
              </div>
              <div className="mt-5 rounded-md border border-border/70 bg-card/50 p-3">
                <p className="text-xs font-medium">What happens on create</p>
                <p className="mt-1 text-[11px] leading-relaxed text-muted-foreground">
                  The route is added to obleth, cache and health settings start with defaults, and this page refreshes with the new model card.
                </p>
              </div>
              <div className="mt-3 flex flex-wrap gap-1.5">
                <Badge className="bg-background text-[10px]">{typeLabel}</Badge>
                <Badge className="bg-background text-[10px]">{hostingLabel}</Badge>
              </div>
            </aside>
          </div>

          <div className="flex flex-col gap-3 border-t border-border/70 bg-background/30 px-5 py-4 sm:flex-row sm:items-center sm:justify-between">
            <Button type="button" variant="ghost" disabled={pending} onClick={onCancel}>
              Cancel
            </Button>
            <div className="flex items-center justify-end gap-2">
              <Button type="button" variant="outline" disabled={pending || step === 0} onClick={() => goToStep(step - 1)}>
                Back
              </Button>
              {step < lastStep ? (
                <Button type="button" disabled={pending} onClick={() => goToStep(step + 1)}>
                  Next
                </Button>
              ) : (
                <Button type="submit" disabled={pending}>
                  {pending ? "Creating..." : "Create model"}
                </Button>
              )}
            </div>
          </div>
        </form>
        )}
      </CardContent>
    </Card>
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

function ModelDetailPanel({
  model,
  summary,
  detail,
  endpoints,
  mcpServers = [],
  isManaged = false,
  pending,
  onCacheToggle,
}: {
  model: ModelRoute;
  summary: ModelHealthSummary;
  detail?: ModelHealthDetail;
  endpoints: ModelEndpoint[];
  mcpServers?: McpServer[];
  isManaged?: boolean;
  pending: boolean;
  onCacheToggle: () => void;
}) {
  const flashSaved = useContext(SaveFlashContext);
  const [editType, setEditType] = useState<string>(model.model_type || "chat");
  const [, healthAction, healthPending] = useActionState(
    async (_prev: ModelActionState | null, formData: FormData) => {
      await setModelHealthConfigAction(formData);
      flashSaved();
      return { ok: true } as ModelActionState;
    },
    null,
  );
  const checks = detail?.checks ?? [];
  const chartData = checks
    .slice()
    .reverse()
    .map((check) => ({
      time: new Date(check.checked_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
      latency: check.latency_ms ?? 0,
      status: check.status,
    }));

  return (
    <Tabs defaultValue="overview">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <TabsList>
          <TabsTrigger value="overview">Overview</TabsTrigger>
          <TabsTrigger value="connection">Connection</TabsTrigger>
          <TabsTrigger value="capabilities">Capabilities</TabsTrigger>
          <TabsTrigger value="capacity">Capacity</TabsTrigger>
          <TabsTrigger value="reliability">
            Reliability
            {endpoints.length > 0 && (
              <span className="tabular-nums opacity-60">{endpoints.length}</span>
            )}
          </TabsTrigger>
          <TabsTrigger value="health">Health</TabsTrigger>
          {isManaged && <TabsTrigger value="provisioning">Provisioning</TabsTrigger>}
        </TabsList>
        <p className="text-[11px] tabular-nums text-muted-foreground">
          {summary.last_checked_at
            ? `Last check ${formatTime(summary.last_checked_at)} / ${summary.last_latency_ms ?? "-"} ms`
            : "Not checked yet"}
        </p>
      </div>

      <TabsContent value="overview">
        <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_minmax(320px,400px)]">
          <div className="space-y-4">
            <PanelCard>
              <div className="flex flex-wrap gap-x-4 gap-y-3 px-4 py-3">
                <SpecItem label="Input">{formatCostPerMillion(model.input_cost_per_token)}</SpecItem>
                <SpecItem label="Output">{formatCostPerMillion(model.output_cost_per_token)}</SpecItem>
                <SpecItem label="Context">{formatNumber(model.context_window)}</SpecItem>
                <SpecItem label="Features">
                  <ChipList items={capabilityList(model)} empty="None declared" />
                </SpecItem>
                <SpecItem label="Boons">
                  <ChipList items={boonList(model)} empty="None" tone="boon" />
                </SpecItem>
                <SpecItem label="Route">
                  <Badge className={model.enabled ? "border-emerald-500/35 bg-emerald-500/10 text-emerald-300" : "border-border bg-muted/30 text-muted-foreground"}>
                    {model.enabled ? "enabled" : "disabled"}
                  </Badge>
                </SpecItem>
                <SpecItem label="Cache">
                  <span className="text-xs font-normal text-muted-foreground">
                    {model.cache_enabled ? `on · ${model.cache_ttl_secs || 300}s` : "off"}
                  </span>
                </SpecItem>
              </div>
            </PanelCard>
          </div>

          <PanelCard
            title="Health trend"
            description="Recent health probe latency"
            actions={<HealthBadge summary={summary} />}
            className="self-start"
          >
            <div className="p-4">
              {chartData.length === 0 ? (
                <div className="flex h-48 items-center justify-center rounded-sm border border-dashed border-border/70 text-xs text-muted-foreground">No checks yet</div>
              ) : (
                <ChartShell heightClass="h-48">
                  <ResponsiveContainer width="100%" height="100%">
                    <LineChart data={chartData} margin={{ top: 8, right: 12, left: 0, bottom: 0 }}>
                      <CartesianGrid {...chartGrid} />
                      <XAxis dataKey="time" tick={axisTick} tickLine={false} axisLine={false} minTickGap={18} />
                      <YAxis tick={axisTick} tickLine={false} axisLine={false} width={44} tickFormatter={compactAxis} />
                      <Tooltip content={tip({ valueFormatter: (v) => `${formatNumber(v)} ms` })} cursor={timeCursor} />
                      <Line type="monotone" dataKey="latency" name="Latency" stroke="hsl(158 42% 48%)" strokeWidth={2} dot={false} isAnimationActive={false} />
                    </LineChart>
                  </ResponsiveContainer>
                </ChartShell>
              )}
              {summary.last_message && (
                <p className="mt-3 rounded-sm border border-border/70 bg-background/40 p-2 text-xs text-muted-foreground">{summary.last_message}</p>
              )}
            </div>
          </PanelCard>
        </div>

        <PanelCard
          title="Live traffic"
          description="Token throughput, latency, and per-tenant breakdown for this model"
          className="mt-4"
        >
          <div className="p-4">
            <ModelMetricsDetail model={model.model_name} />
          </div>
        </PanelCard>
      </TabsContent>

      <TabsContent value="connection">
        <ConnectionTab model={model} editType={editType} setEditType={setEditType} />
      </TabsContent>

      <TabsContent value="capabilities">
        <CapabilitiesTab model={model} editType={editType} mcpServers={mcpServers} />
      </TabsContent>

      <TabsContent value="capacity">
        <TooltipProvider delayDuration={150}>
          <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(320px,400px)]">
            <PanelCard
              title="Capacity & throughput"
              description="Live operational state. Changes apply immediately."
              actions={
                <Badge className={model.enabled ? "border-emerald-500/35 bg-emerald-500/10 text-emerald-300" : "border-border bg-muted/30 text-muted-foreground"}>
                  {model.enabled ? "route enabled" : "route disabled"}
                </Badge>
              }
            >
              <div className="divide-y divide-border/60">
                <SettingRow label="Response cache" hint="Serve repeated identical requests from cache instead of the upstream.">
                  <CacheToggle
                    enabled={model.cache_enabled}
                    ttlSecs={model.cache_ttl_secs || 300}
                    disabled={pending}
                    onToggle={onCacheToggle}
                  />
                </SettingRow>
                <SettingRow label="Admission weight" hint="Relative share of gateway capacity when demand exceeds supply.">
                  <div className="w-44">
                    <ModelWeightControl id={model.id} initial={model.admission_weight} />
                  </div>
                </SettingRow>
                <SettingRow label="Capacity mode" hint="Static uses the max-slots cap below; tuned follows the auto-tune result.">
                  <div className="w-44">
                    <CapacityModeToggle id={model.id} mode={model.capacity_mode} />
                  </div>
                </SettingRow>
                <SettingRow label="Max slots" hint="Hard cap on concurrent in-flight requests to the upstream.">
                  <div className="w-44">
                    <ModelCapacityControl id={model.id} initial={model.max_in_flight} />
                  </div>
                </SettingRow>
                <AutotunePanel model={model} />
              </div>
            </PanelCard>
          </div>
        </TooltipProvider>
      </TabsContent>

      <TabsContent value="reliability">
        <ReliabilityPanel model={model} endpoints={endpoints} pending={pending} />
      </TabsContent>

      <TabsContent value="health">
        <div className="grid gap-4 lg:grid-cols-[minmax(0,380px)_minmax(0,1fr)]">
          <form action={healthAction}>
            <input type="hidden" name="id" value={model.id} />
            <PanelCard
              className="h-full"
              title="Health config"
              description="Probe cadence, alerting and maintenance."
              actions={<SaveButton pending={healthPending} idleLabel="Save" />}
            >
              <div className="grid gap-3 p-4">
                <div className="grid grid-cols-2 gap-3">
                  <Field label="Interval seconds" name="check_interval_secs" type="number" defaultValue={String(summary.check_interval_secs)} />
                  <Field label="Failure threshold" name="failure_threshold" type="number" defaultValue={String(summary.failure_threshold)} />
                </div>
                <Field label="Maintenance until" name="maintenance_until" type="datetime-local" defaultValue={datetimeLocalValue(summary.maintenance_until)} />
                <Field label="Maintenance note" name="maintenance_note" defaultValue={summary.maintenance_note ?? ""} />
                <div className="flex flex-wrap gap-1.5 pt-1">
                  <ChipCheckbox name="checks_enabled" label="Scheduled checks" defaultChecked={summary.checks_enabled} />
                  <ChipCheckbox name="alerts_enabled" label="Slack alerts" defaultChecked={summary.alerts_enabled} />
                </div>
              </div>
            </PanelCard>
          </form>

          <PanelCard
            className="flex h-full flex-col"
            title="Recent checks"
            description="Latest health probe results."
            actions={<HealthBadge summary={summary} />}
          >
            <div className="max-h-80 min-h-0 flex-1 overflow-auto">
              <table className="w-full text-xs">
                <thead className="sticky top-0 bg-card">
                  <tr className="border-b border-border/60 text-left text-[10px] uppercase tracking-wider text-muted-foreground">
                    <th className="py-2 pl-4 pr-3 font-medium">Time</th>
                    <th className="py-2 pr-3 font-medium">Status</th>
                    <th className="py-2 pr-3 font-medium">HTTP</th>
                    <th className="py-2 pr-4 font-medium">Latency</th>
                  </tr>
                </thead>
                <tbody>
                  {checks.map((check) => (
                    <tr key={check.id} className="border-b border-border/40 last:border-b-0">
                      <td className="py-2 pl-4 pr-3 tabular-nums text-muted-foreground">{formatTime(check.checked_at)}</td>
                      <td className="py-2 pr-3"><StatusPill status={check.status} /></td>
                      <td className="py-2 pr-3 tabular-nums text-muted-foreground">{check.http_status ?? "-"}</td>
                      <td className="py-2 pr-4 tabular-nums text-muted-foreground">{check.latency_ms == null ? "-" : `${formatNumber(check.latency_ms)} ms`}</td>
                    </tr>
                  ))}
                  {checks.length === 0 && (
                    <tr>
                      <td colSpan={4} className="py-8 text-center text-muted-foreground">No checks yet</td>
                    </tr>
                  )}
                </tbody>
              </table>
            </div>
            {summary.last_message && (
              <p className="border-t border-border/60 bg-background/30 px-4 py-2.5 text-xs text-muted-foreground">{summary.last_message}</p>
            )}
          </PanelCard>
        </div>
      </TabsContent>

      {isManaged && (
        <TabsContent value="provisioning" className="min-h-0">
          <div className="grid gap-4 lg:h-[calc(100dvh-18rem)] lg:grid-cols-[minmax(0,1fr)_minmax(320px,420px)] lg:overflow-hidden">
            <div className="min-h-0 lg:overflow-y-auto lg:pr-1">
              <ManagedModelConfig modelId={model.id} />
            </div>
            <div className="space-y-3">
              <ProvisionErrorBanner modelId={model.id} />
              <ReplicaPanel modelId={model.id} />
            </div>
          </div>
        </TabsContent>
      )}
    </Tabs>
  );
}

function ConnectionTab({
  model,
  editType,
  setEditType,
}: {
  model: ModelRoute;
  editType: string;
  setEditType: (value: string) => void;
}) {
  const flashSaved = useContext(SaveFlashContext);
  const [state, formAction, pending] = useActionState(
    async (prev: ModelActionState | null, formData: FormData) => {
      const result = await updateModelConnectionAction(prev, formData);
      if (result.ok) flashSaved();
      return result;
    },
    null,
  );

  return (
    <TooltipProvider delayDuration={150}>
      <form action={formAction}>
        <input type="hidden" name="id" value={model.id} />
        <PanelCard
          title="Connection"
          description="Where requests are sent and how this route is billed."
          actions={<SaveButton pending={pending} idleLabel="Save connection" />}
        >
          <div className="grid divide-y divide-border/60 lg:grid-cols-2 lg:divide-x lg:divide-y-0">
            <FormSection title="Upstream" columns={1}>
              <Field label="Upstream model" name="upstream_model" defaultValue={model.upstream_model} required />
              <Field label="API base URL" name="api_base" defaultValue={model.api_base} required />
              <Field label="Upstream API key" name="api_key" placeholder="Leave blank to keep current" />
              <SelectField
                label="Model type"
                name="model_type"
                value={editType}
                onChange={setEditType}
                options={MODEL_TYPE_OPTIONS}
                hint={modelTypeHint(editType)}
              />
              <Field label="Description" name="description" defaultValue={model.description} />
              <ChipGroup label="Status">
                <ChipCheckbox name="enabled" label="Route enabled" defaultChecked={model.enabled} />
              </ChipGroup>
            </FormSection>

            <FormSection title="Cost" columns={1}>
              {(editType === "chat" || editType === "embedding") && (
                <Field label="Input cost / token" name="input_cost_per_token" defaultValue={toPlainDecimal(model.input_cost_per_token)} />
              )}
              {editType === "chat" && (
                <Field label="Output cost / token" name="output_cost_per_token" defaultValue={toPlainDecimal(model.output_cost_per_token)} />
              )}
              {editType === "image" && (
                <Field label="Cost / image" name="cost_per_image" defaultValue={toPlainDecimal(model.cost_per_image)} />
              )}
              {editType === "audio_speech" && (
                <Field label="Cost / character" name="cost_per_character" defaultValue={toPlainDecimal(model.cost_per_character)} />
              )}
              {editType === "audio_transcription" && (
                <Field label="Cost / audio second" name="cost_per_audio_second" defaultValue={toPlainDecimal(model.cost_per_audio_second)} />
              )}
            </FormSection>
          </div>
          {state?.ok === false && (
            <p className="border-t border-border/60 bg-destructive/10 px-4 py-2 text-xs text-destructive">{state.error}</p>
          )}
        </PanelCard>
      </form>
    </TooltipProvider>
  );
}

function CapabilitiesTab({
  model,
  editType,
  mcpServers = [],
}: {
  model: ModelRoute;
  editType: string;
  mcpServers?: McpServer[];
}) {
  const flashSaved = useContext(SaveFlashContext);
  const [state, formAction, pending] = useActionState(
    async (prev: ModelActionState | null, formData: FormData) => {
      const result = await updateModelCapabilitiesAction(prev, formData);
      if (result.ok) flashSaved();
      return result;
    },
    null,
  );

  if (editType !== "chat" && editType !== "embedding") {
    return (
      <PanelCard title="Capabilities" description="Routing capabilities apply to chat and embedding models.">
        <p className="px-4 py-6 text-xs text-muted-foreground">
          No routing capabilities for {MODEL_TYPE_LABELS[editType] ?? editType} models.
        </p>
      </PanelCard>
    );
  }

  return (
    <TooltipProvider delayDuration={150}>
      <form action={formAction}>
        <input type="hidden" name="id" value={model.id} />
        <PanelCard
          title="Capabilities & routing"
          description="What the model supports and how the auto router treats it."
          actions={<SaveButton pending={pending} idleLabel="Save capabilities" />}
        >
          <div className="grid gap-4 p-4">
            <Field label="Context window" name="context_window" type="number" defaultValue={String(model.context_window)} />
            {editType === "chat" && (
              <ChatCapabilityFields model={model} mcpServers={mcpServers} />
            )}
          </div>
          {state?.ok === false && (
            <p className="border-t border-border/60 bg-destructive/10 px-4 py-2 text-xs text-destructive">{state.error}</p>
          )}
        </PanelCard>
      </form>
    </TooltipProvider>
  );
}

export function ModelCapacityControl({ id, initial }: { id: string; initial: number | null }) {
  const flashSaved = useContext(SaveFlashContext);
  const [value, setValue] = useState(initial == null ? "" : String(initial));
  const [pending, start] = useTransition();
  useEffect(() => {
    setValue(initial == null ? "" : String(initial));
  }, [initial]);
  const next = value.trim() === "" ? null : Math.max(1, Math.round(Number(value)) || 1);
  const changed = next !== initial;

  const apply = () =>
    start(async () => {
      await setModelCapacityAction(id, next);
      flashSaved();
    });

  return (
    <div className="grid w-full grid-cols-[minmax(0,1fr)_auto] gap-2">
      <Input
        type="number"
        min={1}
        aria-label="Model max in-flight slots"
        placeholder="No cap"
        className="h-8 min-w-0 text-xs"
        value={value}
        onChange={(e) => setValue(e.target.value)}
      />
      <SaveButton type="button" onClick={apply} pending={pending} idleLabel="Apply" variant="secondary" disabled={!changed} />
    </div>
  );
}

export function CapacityModeToggle({ id, mode }: { id: string; mode: string }) {
  const [pending, start] = useTransition();
  const current = mode === "tuned" ? "tuned" : "static";
  const set = (next: "static" | "tuned") => {
    if (next === current) return;
    start(() => setModelCapacityModeAction(id, next));
  };
  return (
    <div className="inline-flex w-full overflow-hidden rounded-md border border-border" role="group" aria-label="Capacity mode">
      {(["static", "tuned"] as const).map((opt) => (
        <button
          key={opt}
          type="button"
          disabled={pending}
          onClick={() => set(opt)}
          className={cn(
            "flex-1 px-2 py-1 text-xs capitalize transition-colors disabled:opacity-60",
            current === opt ? "bg-primary text-primary-foreground" : "bg-transparent text-muted-foreground hover:bg-muted/50",
          )}
        >
          {opt}
        </button>
      ))}
    </div>
  );
}

const AUTOTUNE_KNEE_LABEL: Record<AutotuneReport["knee_reason"], string> = {
  latency_degraded: "Latency degraded past your tolerance",
  plateau: "Throughput plateaued",
  max_concurrency: "Reached the concurrency ceiling (real knee may be higher)",
  no_data: "No usable samples — upstream unreachable",
};

const AUTOTUNE_HEADROOM_OPTIONS = [
  { value: "2", label: "Tight — 2× a single request" },
  { value: "4", label: "Balanced — 4× a single request" },
  { value: "8", label: "Relaxed — 8× a single request" },
] as const;

function AutotuneField({
  label,
  info,
  children,
}: {
  label: string;
  info: string;
  children: ReactNode;
}) {
  return (
    <div className="flex items-center gap-3 p-3">
      <div className="flex w-40 shrink-0 items-center gap-1.5">
        <Label className="text-xs">{label}</Label>
        <UiTooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              aria-label={`About ${label}`}
              className="text-muted-foreground/70 transition-colors hover:text-foreground"
            >
              <Info className="h-3.5 w-3.5" />
            </button>
          </TooltipTrigger>
          <TooltipContent side="left" align="center" className="max-w-xs leading-relaxed">
            {info}
          </TooltipContent>
        </UiTooltip>
      </div>
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

export function AutotunePanel({ model }: { model: ModelRoute }) {
  const probeable = model.model_type === "chat" || model.model_type === "embedding";
  const [open, setOpen] = useState(false);
  const [headroom, setHeadroom] = useState("4");
  const [replicas, setReplicas] = useState("1");
  const [workload, setWorkload] = useState<AutotuneWorkload>("chat");
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [report, setReport] = useState<AutotuneReport | null>(null);
  const [pending, start] = useTransition();

  if (!probeable) return null;

  const reset = () => {
    setReport(null);
    setError(null);
  };

  const runProbe = async () => {
    setRunning(true);
    setError(null);
    setReport(null);
    try {
      const result = await autotuneModelAction(model.id, {
        latency_headroom: Math.max(1.5, Number(headroom) || 4),
        replicas: Math.max(1, Math.round(Number(replicas) || 1)),
        workload,
      });
      setReport(result);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Auto-tune failed");
    } finally {
      setRunning(false);
    }
  };

  const apply = () => {
    if (!report) return;
    start(async () => {
      await applyAutotuneCapacityAction(model.id, report.recommended_max_in_flight);
      setOpen(false);
      reset();
    });
  };

  return (
    <SettingRow
      label="Auto-tune capacity"
      hint="Ramp live load against the upstream to find the in-flight knee. Best for self-hosted models."
    >
      <Dialog
        open={open}
        onOpenChange={(next) => {
          setOpen(next);
          if (!next) reset();
        }}
      >
        <DialogTrigger asChild>
          <Button type="button" size="sm" variant="secondary">
            <Zap className="h-3.5 w-3.5" />
            Auto-tune
          </Button>
        </DialogTrigger>
        <DialogContent className="max-w-3xl">
          <DialogHeader>
            <div className="flex items-center gap-2">
              <DialogTitle>Auto-tune {model.model_name}</DialogTitle>
              <TooltipProvider delayDuration={150}>
                <UiTooltip>
                  <TooltipTrigger asChild>
                    <button
                      type="button"
                      aria-label="How auto-tune works"
                      className="text-muted-foreground transition-colors hover:text-foreground"
                    >
                      <Info className="h-4 w-4" />
                    </button>
                  </TooltipTrigger>
                  <TooltipContent side="bottom" align="start" className="max-w-xs leading-relaxed">
                    Sends real requests straight to the upstream and ramps concurrency to find its
                    true capacity. Hover each field below for what it controls.
                  </TooltipContent>
                </UiTooltip>
              </TooltipProvider>
            </div>
            <DialogDescription>
              Finds the in-flight knee by ramping live load.{" "}
              <span className="font-medium text-foreground">Consumes upstream tokens</span> &mdash;
              avoid running against a busy production model.
            </DialogDescription>
          </DialogHeader>

          <TooltipProvider delayDuration={150}>
            <div className="divide-y divide-border rounded-md border border-border">
              <AutotuneField
                label="Replicas running"
                info="How many copies of the model are serving traffic. The probe scales its concurrency ceiling (up to ~32× this) so the ramp covers a realistic load for your fleet."
              >
                <Input
                  type="number"
                  min={1}
                  step={1}
                  value={replicas}
                  onChange={(e) => setReplicas(e.target.value)}
                  className="h-9 w-full text-xs"
                />
              </AutotuneField>

              <AutotuneField
                label="Latency tolerance"
                info="How much slower than a single idle request you'll accept at peak load. The ramp stops once p99 crosses this multiple of the baseline — tighter tolerance means fewer slots but snappier responses."
              >
                <Select
                  value={headroom}
                  onChange={(e) => setHeadroom(e.target.value)}
                  className="h-9 w-full text-xs"
                >
                  {AUTOTUNE_HEADROOM_OPTIONS.map((o) => (
                    <option key={o.value} value={o.value}>
                      {o.label}
                    </option>
                  ))}
                </Select>
              </AutotuneField>

              <AutotuneField
                label="Workload"
                info="The shape of the probe requests. Pick whichever matches real traffic — coding sends large prompts and longer replies, which costs more capacity than short chat turns, so it tunes to a lower slot count."
              >
                <Select
                  value={workload}
                  onChange={(e) => setWorkload(e.target.value as AutotuneWorkload)}
                  className="h-9 w-full text-xs"
                >
                  <option value="chat">Chat — short prompt, short reply</option>
                  <option value="coding">Coding — large context, longer reply</option>
                </Select>
              </AutotuneField>
            </div>
          </TooltipProvider>

          {error && (
            <p className="rounded-sm border border-destructive/40 bg-destructive/10 p-2 text-xs text-destructive">{error}</p>
          )}

          {report && (
            <div className="space-y-3">
              <div className="flex flex-wrap items-center gap-x-6 gap-y-1 rounded-md border border-border bg-card/40 p-3 text-xs">
                <div>
                  <span className="text-muted-foreground">Recommended slots</span>
                  <p className="text-lg font-semibold tabular-nums">{report.recommended_max_in_flight}</p>
                </div>
                <div>
                  <span className="text-muted-foreground">Throughput at knee</span>
                  <p className="text-lg font-semibold tabular-nums">{report.recommended_throughput_rps.toFixed(1)} rps</p>
                </div>
                <div>
                  <span className="text-muted-foreground">Latency budget</span>
                  <p className="font-medium tabular-nums">
                    {report.baseline_p99_ms > 0
                      ? `${report.baseline_p99_ms} ms → ${report.latency_ceiling_ms} ms (${report.latency_headroom.toFixed(0)}×)`
                      : "no baseline"}
                  </p>
                </div>
                <div className="min-w-[12rem] flex-1">
                  <span className="text-muted-foreground">Why it stopped</span>
                  <p className="font-medium">{AUTOTUNE_KNEE_LABEL[report.knee_reason]}</p>
                </div>
              </div>

              <div className="max-h-56 overflow-auto rounded-md border border-border">
                <table className="w-full text-xs">
                  <thead className="sticky top-0 bg-card">
                    <tr className="border-b border-border text-left text-muted-foreground">
                      <th className="py-2 pl-3 pr-3 font-medium">Concurrency</th>
                      <th className="py-2 pr-3 font-medium">Throughput</th>
                      <th className="py-2 pr-3 font-medium">p50</th>
                      <th className="py-2 pr-3 font-medium">p99</th>
                      <th className="py-2 pr-3 font-medium">Errors</th>
                    </tr>
                  </thead>
                  <tbody>
                    {report.steps.map((step) => {
                      const isRec = step.concurrency === report.recommended_max_in_flight;
                      return (
                        <tr key={step.concurrency} className={cn("border-b border-border/50", isRec && "bg-primary/10")}>
                          <td className="py-1.5 pl-3 pr-3 tabular-nums">
                            {step.concurrency}
                            {isRec && <span className="ml-1 text-primary">★</span>}
                          </td>
                          <td className="py-1.5 pr-3 tabular-nums">{step.throughput_rps.toFixed(1)} rps</td>
                          <td className="py-1.5 pr-3 tabular-nums text-muted-foreground">{step.p50_ms} ms</td>
                          <td className="py-1.5 pr-3 tabular-nums text-muted-foreground">{step.p99_ms} ms</td>
                          <td className="py-1.5 pr-3 tabular-nums text-muted-foreground">{step.errors}</td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            </div>
          )}

          <DialogFooter>
            {!report ? (
              <Button type="button" size="sm" onClick={runProbe} disabled={running}>
                {running ? (
                  <>
                    <RefreshCw className="h-3.5 w-3.5 animate-spin" />
                    Probing…
                  </>
                ) : (
                  <>
                    <Zap className="h-3.5 w-3.5" />
                    Run probe
                  </>
                )}
              </Button>
            ) : (
              <div className="flex w-full items-center justify-end gap-2">
                <Button type="button" size="sm" variant="secondary" onClick={runProbe} disabled={running || pending}>
                  Re-run
                </Button>
                <Button
                  type="button"
                  size="sm"
                  onClick={apply}
                  disabled={pending || report.knee_reason === "no_data"}
                >
                  {pending ? "Applying…" : `Apply ${report.recommended_max_in_flight} slots`}
                </Button>
              </div>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </SettingRow>
  );
}

// Save/apply button: the label and width stay fixed; only the icon swaps to a
// spinner while the action runs, and the button is disabled until it completes.
// Success is signalled on the surrounding model card, not on the button.
function SaveButton({
  pending,
  idleLabel = "Save",
  size = "sm",
  variant = "default",
  disabled,
  type = "submit",
  onClick,
}: {
  pending: boolean;
  idleLabel?: string;
  size?: "sm" | "default";
  variant?: "default" | "secondary";
  disabled?: boolean;
  type?: "submit" | "button";
  onClick?: () => void;
}) {
  return (
    <Button type={type} onClick={onClick} size={size} variant={variant} disabled={pending || disabled} aria-busy={pending}>
      {pending ? (
        <RefreshCw className="h-3.5 w-3.5 animate-spin" aria-hidden />
      ) : (
        <Save className="h-3.5 w-3.5" aria-hidden />
      )}
      {idleLabel}
    </Button>
  );
}

// Small info affordance that reveals help text on hover. Replaces inline hint
// paragraphs so labels stay short and scannable.
function InfoTooltip({ text, side = "top" }: { text: string; side?: "top" | "right" | "bottom" | "left" }) {
  return (
    <UiTooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          aria-label="More info"
          className="text-muted-foreground/70 transition-colors hover:text-foreground"
        >
          <Info className="h-3.5 w-3.5" />
        </button>
      </TooltipTrigger>
      <TooltipContent side={side} align="center" className="max-w-xs leading-relaxed">
        {text}
      </TooltipContent>
    </UiTooltip>
  );
}

export function ModelWeightControl({ id, initial }: { id: string; initial: number }) {
  const flashSaved = useContext(SaveFlashContext);
  const [value, setValue] = useState(String(initial));
  const [pending, start] = useTransition();
  useEffect(() => {
    setValue(String(initial));
  }, [initial]);
  const next = Math.max(1, Math.round(Number(value)) || 1);
  const changed = next !== initial;

  const apply = () =>
    start(async () => {
      await setModelWeightAction(id, next);
      flashSaved();
    });

  return (
    <div className="grid w-full grid-cols-[minmax(0,1fr)_auto] gap-2">
      <Input
        type="number"
        min={1}
        aria-label="Model admission weight"
        className="h-8 min-w-0 text-xs"
        value={value}
        onChange={(e) => setValue(e.target.value)}
      />
      <SaveButton type="button" onClick={apply} pending={pending} idleLabel="Apply" variant="secondary" disabled={!changed} />
    </div>
  );
}

// Ghosted provider backdrop — large mark in the row's right dead zone with a
// soft fade so foreground text stays crisp.
function ModelProviderBackdrop({
  name,
  upstream,
  selected = false,
  variant = "row",
}: {
  name: string;
  upstream?: string;
  selected?: boolean;
  variant?: "row" | "detail";
}) {
  const provider = providerForModel(name, upstream);
  const [failed, setFailed] = useState(false);
  const letter = name.replace(/[^a-z0-9]/gi, "").slice(0, 1).toUpperCase() || "?";

  const mark =
    provider && !failed ? (
      // eslint-disable-next-line @next/next/no-img-element
      <img
        src={provider.src}
        alt=""
        className={cn(
          "provider-logo-backdrop object-contain",
          variant === "row" ? "h-24 w-24 md:h-28 md:w-28" : "h-44 w-44 md:h-52 md:w-52",
        )}
        onError={() => setFailed(true)}
      />
    ) : (
      <span
        className={cn(
          "provider-logo-backdrop font-bold uppercase text-muted-foreground",
          variant === "row" ? "text-6xl md:text-7xl" : "text-8xl md:text-9xl",
        )}
      >
        {letter}
      </span>
    );

  if (variant === "detail") {
    return (
      <div aria-hidden className="pointer-events-none absolute -bottom-10 -right-6 z-0 hidden select-none md:block">
        <div className={cn("provider-logo-backdrop-shell", selected && "is-selected")}>{mark}</div>
      </div>
    );
  }

  return (
    <>
      <div
        aria-hidden
        className={cn(
          "pointer-events-none absolute inset-y-0 left-0 z-[1] hidden w-[58%] md:block",
          selected
            ? "bg-gradient-to-r from-muted/45 from-[38%] via-muted/15 to-transparent"
            : "bg-gradient-to-r from-card/95 from-[38%] via-card/45 to-transparent",
        )}
      />
      <div
        aria-hidden
        className="pointer-events-none absolute right-[4.25rem] top-1/2 z-0 hidden -translate-y-1/2 select-none md:block"
      >
        <div className={cn("provider-logo-backdrop-shell", selected && "is-selected")}>{mark}</div>
      </div>
    </>
  );
}

function HealthCell({ summary }: { summary: ModelHealthSummary }) {
  const maintenance = isInMaintenance(summary);
  return (
    <div className="space-y-1">
      <div className="flex items-center gap-2">
        <HealthBadge summary={summary} />
        {maintenance && <Badge className="border-amber-500/35 bg-amber-500/10 text-amber-300">maintenance</Badge>}
      </div>
      <p className="text-[11px] tabular-nums text-muted-foreground">
        {summary.last_checked_at ? `${formatTime(summary.last_checked_at)} / ${summary.last_latency_ms ?? "-"} ms` : "not checked"}
      </p>
      {summary.alert_state === "firing" && <p className="text-[11px] text-destructive">alert firing</p>}
    </div>
  );
}

function HealthBadge({ summary }: { summary: ModelHealthSummary }) {
  const status = isInMaintenance(summary) ? "maintenance" : summary.status;
  const icon =
    status === "healthy" ? <Check className="h-3.5 w-3.5" /> :
    status === "unhealthy" ? <XCircle className="h-3.5 w-3.5" /> :
    status === "maintenance" ? <PauseCircle className="h-3.5 w-3.5" /> :
    <HeartPulse className="h-3.5 w-3.5" />;
  return (
    <Badge className={cn("gap-1.5", healthClass(status))}>
      {icon}
      {status}
    </Badge>
  );
}

function StatusPill({ status }: { status: string }) {
  return <span className={cn("rounded-sm px-2 py-0.5 text-[11px]", healthClass(status))}>{status}</span>;
}

function healthClass(status: string) {
  if (status === "healthy") return "border-emerald-500/35 bg-emerald-500/10 text-emerald-300";
  if (status === "unhealthy") return "border-red-500/35 bg-red-500/10 text-red-300";
  if (status === "degraded") return "border-amber-500/35 bg-amber-500/10 text-amber-300";
  if (status === "maintenance") return "border-amber-500/35 bg-amber-500/10 text-amber-300";
  if (status === "disabled") return "border-border bg-muted/30 text-muted-foreground";
  return "border-border bg-background text-muted-foreground";
}

function ReliabilityPanel({
  model,
  endpoints,
  pending,
}: {
  model: ModelRoute;
  endpoints: ModelEndpoint[];
  pending: boolean;
}) {
  const flashSaved = useContext(SaveFlashContext);
  const [busy, start] = useTransition();
  const disabled = pending || busy;
  const addFormRef = useRef<HTMLFormElement>(null);

  function saveReliability(formData: FormData) {
    const rawTimeout = String(formData.get("request_timeout_secs") ?? "").trim();
    const body = {
      request_timeout_secs: rawTimeout === "" ? null : Number(rawTimeout),
      max_retries: Number(formData.get("max_retries") ?? 0),
      retry_backoff_ms: Number(formData.get("retry_backoff_ms") ?? 200),
      endpoint_selection_mode: String(formData.get("endpoint_selection_mode") ?? "failover"),
    };
    start(async () => {
      await setModelReliabilityAction(model.id, body);
      flashSaved();
    });
  }

  function addEndpoint(formData: FormData) {
    start(async () => {
      await createModelEndpointAction(model.id, formData);
      addFormRef.current?.reset();
    });
  }

  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,380px)_minmax(0,1fr)]">
      <form action={saveReliability}>
        <PanelCard
          className="h-full"
          title="Delivery"
          description="Per-request timeout and retry behaviour."
          actions={<SaveButton pending={busy} idleLabel="Save" disabled={pending} />}
        >
          <div className="grid gap-3 p-4">
            <Field
              label="Request timeout (s)"
              name="request_timeout_secs"
              type="number"
              placeholder="default"
              defaultValue={model.request_timeout_secs == null ? "" : String(model.request_timeout_secs)}
            />
            <div className="grid grid-cols-2 gap-3">
              <Field label="Max retries" name="max_retries" type="number" defaultValue={String(model.max_retries)} />
              <Field label="Retry backoff (ms)" name="retry_backoff_ms" type="number" defaultValue={String(model.retry_backoff_ms)} />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor={`selection-${model.id}`}>Endpoint selection</Label>
              <select
                id={`selection-${model.id}`}
                name="endpoint_selection_mode"
                defaultValue={model.endpoint_selection_mode}
                className="flex h-9 w-full rounded-md border border-input bg-background px-3 py-1 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
              >
                <option value="failover">failover (priority order)</option>
                <option value="load_balance">load_balance (weighted)</option>
                <option value="session_hash">session_hash (sticky by session)</option>
              </select>
            </div>
          </div>
        </PanelCard>
      </form>

      <PanelCard
        className="flex h-full flex-col"
        title="Endpoints"
        description="Additional upstream clusters serving this model."
      >
        {endpoints.length === 0 ? (
          <div className="flex flex-1 items-center justify-center px-4 py-8">
            <p className="text-xs text-muted-foreground">
              No explicit endpoints. Requests use the model&apos;s primary API base.
            </p>
          </div>
        ) : (
          <div className="flex-1 overflow-x-auto">
            <table className="w-full text-left text-xs">
              <thead className="text-[10px] uppercase tracking-wider text-muted-foreground">
                <tr className="border-b border-border/60">
                  <th className="py-2 pl-4 pr-3 font-medium">Name</th>
                  <th className="py-2 pr-3 font-medium">API base</th>
                  <th className="py-2 pr-3 font-medium">Priority</th>
                  <th className="py-2 pr-3 font-medium">Weight</th>
                  <th className="py-2 pr-3 font-medium">Health</th>
                  <th className="py-2 pr-4" />
                </tr>
              </thead>
              <tbody>
                {endpoints.map((ep) => (
                  <tr key={ep.id} className="border-b border-border/40 last:border-b-0">
                    <td className="py-2 pl-4 pr-3 font-medium">{ep.name}</td>
                    <td className="py-2 pr-3 font-mono text-[11px] text-muted-foreground">{ep.api_base}</td>
                    <td className="py-2 pr-3 tabular-nums">{ep.priority}</td>
                    <td className="py-2 pr-3 tabular-nums">{ep.weight}</td>
                    <td className="py-2 pr-3">
                      <StatusPill status={ep.enabled ? ep.health_status : "disabled"} />
                    </td>
                    <td className="py-2 pr-4">
                      <div className="flex items-center justify-end gap-1.5">
                        <Button
                          type="button"
                          size="sm"
                          variant="ghost"
                          disabled={disabled}
                          onClick={() =>
                            start(() =>
                              updateModelEndpointAction(model.id, ep.id, {
                                name: ep.name,
                                api_base: ep.api_base,
                                priority: ep.priority,
                                weight: ep.weight,
                                enabled: !ep.enabled,
                              }),
                            )
                          }
                        >
                          {ep.enabled ? "Disable" : "Enable"}
                        </Button>
                        <Button
                          type="button"
                          size="sm"
                          variant="ghost"
                          disabled={disabled}
                          onClick={() => {
                            if (!window.confirm(`Remove endpoint "${ep.name}"?`)) return;
                            start(() => deleteModelEndpointAction(model.id, ep.id));
                          }}
                        >
                          <Trash2 className="h-3.5 w-3.5" />
                        </Button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        <form ref={addFormRef} action={addEndpoint} className="border-t border-border/60 bg-background/30 px-4 py-4">
          <p className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">Add endpoint</p>
          <div className="mt-3 grid gap-3 md:grid-cols-2">
            <Field label="Name" name="name" placeholder="cluster-b" required />
            <Field label="API base URL" name="api_base" placeholder="http://cluster-b/v1" required />
            <Field label="API key" name="api_key" placeholder="Leave blank to inherit" />
            <div className="grid grid-cols-2 gap-3">
              <Field label="Priority" name="priority" type="number" defaultValue="100" />
              <Field label="Weight" name="weight" type="number" defaultValue="100" />
            </div>
          </div>
          <Button type="submit" size="sm" variant="secondary" disabled={disabled} className="mt-3">
            <Plus className="h-3.5 w-3.5" />
            Add endpoint
          </Button>
        </form>
      </PanelCard>
    </div>
  );
}

// Bordered section with an optional header row; the shared container for every
// block inside the model detail tabs so they all read as one design system.
function PanelCard({
  title,
  description,
  actions,
  className,
  children,
}: {
  title?: string;
  description?: string;
  actions?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  return (
    <section className={cn("overflow-hidden rounded-lg border border-border bg-card/40", className)}>
      {title && (
        <header className="flex items-center justify-between gap-3 border-b border-border/60 bg-background/30 px-4 py-2.5">
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium">{title}</p>
            {description && <p className="text-xs text-muted-foreground">{description}</p>}
          </div>
          {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
        </header>
      )}
      {children}
    </section>
  );
}

// One labeled row in a settings list: label + hint on the left, the control on
// the right. Stack these inside `divide-y` instead of tiling identical boxes.
function SettingRow({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <div className="flex flex-wrap items-center justify-between gap-x-6 gap-y-2 px-4 py-3">
      <div className="min-w-0 max-w-sm">
        <p className="text-xs font-medium">{label}</p>
        {hint && <p className="mt-0.5 text-[11px] leading-snug text-muted-foreground">{hint}</p>}
      </div>
      <div className="flex min-w-0 shrink-0 items-center gap-2">{children}</div>
    </div>
  );
}

// Inline label-over-value pair for the overview spec strip. Values render in
// full (chips for lists) instead of truncating inside fixed-width boxes.
function SpecItem({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="min-w-0 border-l border-border/60 pl-4 first:border-l-0 first:pl-0">
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      <div className="mt-1 text-sm font-medium tabular-nums">{children}</div>
    </div>
  );
}

// Titled slice of a form inside a PanelCard. Borderless on its own; the parent
// applies `divide-y` / `divide-x` so sections work stacked or side by side.
function FormSection({ title, columns = 2, children }: { title: string; columns?: 1 | 2; children: ReactNode }) {
  return (
    <div className="min-w-0 px-4 py-4">
      <p className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">{title}</p>
      <div className={cn("mt-3 grid gap-x-4 gap-y-3", columns === 2 && "md:grid-cols-2")}>{children}</div>
    </div>
  );
}

// A labeled cluster of chip checkboxes. `info` renders an info tooltip beside the
// label; the legacy `hint` still renders inline when provided.
function ChipGroup({ label, hint, info, children }: { label: string; hint?: string; info?: string; children: ReactNode }) {
  return (
    <div>
      <div className="flex items-center gap-1.5">
        <p className="text-xs font-medium">{label}</p>
        {info && <InfoTooltip text={info} />}
      </div>
      {hint && <p className="mt-0.5 max-w-prose text-[11px] leading-snug text-muted-foreground">{hint}</p>}
      <div className="mt-2 flex flex-wrap gap-1.5">{children}</div>
    </div>
  );
}

// Checkbox dressed as a selectable chip. Keeps native form semantics (the
// hidden input still submits) while reading as a tag picker instead of a
// wall of checkboxes. Supports an optional controlled mode (`checked` +
// `onChange`) for fields whose state drives other fields, and `disabled` for
// gated options.
function ChipCheckbox({
  name,
  label,
  defaultChecked,
  checked,
  onChange,
  disabled,
  hint,
}: {
  name: string;
  label: string;
  defaultChecked?: boolean;
  checked?: boolean;
  onChange?: (checked: boolean) => void;
  disabled?: boolean;
  hint?: string;
}) {
  const controlled = checked !== undefined;
  return (
    <label title={hint} className={cn(disabled ? "cursor-not-allowed" : "cursor-pointer")}>
      <input
        type="checkbox"
        name={name}
        disabled={disabled}
        className="peer sr-only"
        {...(controlled
          ? { checked, onChange: (e: ChangeEvent<HTMLInputElement>) => onChange?.(e.target.checked) }
          : { defaultChecked })}
      />
      <span
        className={cn(
          "inline-flex items-center gap-1.5 rounded-md border border-border bg-transparent px-2 py-1 text-xs font-medium text-muted-foreground transition-colors",
          "hover:bg-accent hover:text-accent-foreground",
          "peer-checked:bg-secondary peer-checked:text-foreground",
          "peer-focus-visible:ring-1 peer-focus-visible:ring-ring",
          "[&>svg]:hidden peer-checked:[&>svg]:block",
          disabled && "pointer-events-none opacity-40",
        )}
      >
        <Check className="h-3 w-3" strokeWidth={2.5} />
        {label}
      </span>
    </label>
  );
}

// Native capabilities, routing tags, boons, and MCP tool grants for chat
// routes, shared by the create wizard and the edit panel. Tool grants depend
// on native function calling + tool choice: without them the gateway can't run
// the tool loop and silently drops the tools (the model then claims it can't
// search). Rather than letting that misconfiguration through, the Tools group
// is disabled until both capabilities are on, and any existing grants are
// cleared the moment either is turned off.
function ChatCapabilityFields({ model, mcpServers }: { model?: ModelRoute; mcpServers: McpServer[] }) {
  const [fnCalling, setFnCalling] = useState(model?.supports_function_calling ?? false);
  const [toolChoice, setToolChoice] = useState(model?.supports_tool_choice ?? false);
  const [granted, setGranted] = useState<Set<string>>(() => new Set(model?.tool_servers ?? []));
  const toolsReady = fnCalling && toolChoice;

  useEffect(() => {
    if (!toolsReady) {
      setGranted((prev) => (prev.size === 0 ? prev : new Set()));
    }
  }, [toolsReady]);

  return (
    <>
      <ChipGroup label="Native capabilities" info="What the model natively supports. These gate request features and routing.">
        <ChipCheckbox name="supports_function_calling" label="Function calling" checked={fnCalling} onChange={setFnCalling} />
        <ChipCheckbox name="supports_system_messages" label="System messages" defaultChecked={model ? model.supports_system_messages : true} />
        <ChipCheckbox name="supports_response_schema" label="Response schema" defaultChecked={model?.supports_response_schema ?? false} />
        <ChipCheckbox name="supports_tool_choice" label="Tool choice" checked={toolChoice} onChange={setToolChoice} />
      </ChipGroup>
      <ChipGroup label="Routing tags" info="Hints the auto router matches against request intent. The “vision” tag marks native image support.">
        {MODEL_TAGS.map((tag) => (
          <ChipCheckbox
            key={tag}
            name={`tag_${tag}`}
            label={tag}
            defaultChecked={model ? (model.tags?.includes(tag) ?? false) || (tag === "vision" && model.supports_vision) : false}
          />
        ))}
      </ChipGroup>
      <ChipGroup label="Boons" info="Gateway capabilities granted that the model lacks natively. Configure in Settings → Boons.">
        {MODEL_BOONS.map((boon) => (
          <ChipCheckbox
            key={boon.value}
            name={`boon_${boon.value}`}
            label={boon.label}
            hint={boon.description}
            defaultChecked={model?.boons?.includes(boon.value) ?? false}
          />
        ))}
      </ChipGroup>
      <ChipGroup
        label="Tools"
        info="Registered MCP servers whose tools this model may use. The gateway runs the tool loop itself, which needs native function calling + tool choice."
      >
        {mcpServers.length === 0 ? (
          <p className="text-xs text-muted-foreground">No MCP servers registered. Add one on the MCP page first.</p>
        ) : !toolsReady ? (
          <p className="max-w-prose text-[11px] leading-snug text-amber-500/90">
            Enable <span className="font-medium text-foreground">Function calling</span> and{" "}
            <span className="font-medium text-foreground">Tool choice</span> above to grant tools — the gateway’s tool
            loop can’t run without them.
          </p>
        ) : (
          mcpServers.map((server) => (
            <ChipCheckbox
              key={server.id}
              name={`tool_server_${server.name}`}
              label={server.name}
              hint={`Grant this model the tools served by ${server.name} (${server.upstream_url}).`}
              checked={granted.has(server.name)}
              onChange={(c) =>
                setGranted((prev) => {
                  const next = new Set(prev);
                  if (c) next.add(server.name);
                  else next.delete(server.name);
                  return next;
                })
              }
            />
          ))
        )}
      </ChipGroup>
    </>
  );
}

function CacheToggle({
  enabled,
  ttlSecs,
  disabled,
  onToggle,
}: {
  enabled: boolean;
  ttlSecs: number;
  disabled?: boolean;
  onToggle: () => void;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onToggle}
      aria-pressed={enabled}
      title={enabled ? `Response cache on (${ttlSecs}s TTL) - click to disable` : "Click to enable response cache"}
      className={cn(
        buttonVariants({ variant: enabled ? "secondary" : "outline", size: "sm" }),
        "h-8 gap-1.5 font-medium tabular-nums",
        enabled && "border-emerald-500/35 bg-emerald-500/10 text-emerald-300 hover:bg-emerald-500/15",
      )}
    >
      {enabled ? (
        <>
          <Check className="h-3.5 w-3.5 shrink-0" strokeWidth={2.5} />
          <span>Cached</span>
          <span className="font-normal text-emerald-300/70">{ttlSecs}s</span>
        </>
      ) : (
        <>
          <Database className="h-3.5 w-3.5 shrink-0 opacity-60" />
          <span className="text-muted-foreground">Enable</span>
        </>
      )}
    </button>
  );
}

function Field({
  label,
  name,
  placeholder,
  required,
  type = "text",
  defaultValue,
}: {
  label: string;
  name: string;
  placeholder?: string;
  required?: boolean;
  type?: string;
  defaultValue?: string;
}) {
  return (
    <div className="space-y-1.5">
      <Label htmlFor={`${name}-${label}`}>{label}</Label>
      <Input id={`${name}-${label}`} name={name} type={type} placeholder={placeholder} required={required} defaultValue={defaultValue} />
    </div>
  );
}

function modelTypeHint(type: string): string {
  switch (type) {
    case "chat":
      return "Serves /v1/chat/completions and /v1/completions. Billed per token; eligible for `auto` routing.";
    case "embedding":
      return "Serves /v1/embeddings. Billed per input token.";
    case "audio_transcription":
      return "Serves /v1/audio/transcriptions and /v1/audio/translations (multipart audio upload).";
    case "audio_speech":
      return "Serves /v1/audio/speech. Billed per input character.";
    case "image":
      return "Serves /v1/images/generations. Billed per image.";
    default:
      return "";
  }
}

function SelectField({
  label,
  name,
  value,
  onChange,
  options,
  hint,
}: {
  label: string;
  name: string;
  value: string;
  onChange?: (value: string) => void;
  options: readonly { value: string; label: string }[];
  hint?: string;
}) {
  return (
    <div className="space-y-1.5">
      <Label htmlFor={`${name}-${label}`}>{label}</Label>
      <select
        id={`${name}-${label}`}
        name={name}
        value={value}
        onChange={(e) => onChange?.(e.target.value)}
        className="flex h-9 w-full rounded-md border border-input bg-background px-3 py-1 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
      >
        {options.map((opt) => (
          <option key={opt.value} value={opt.value}>
            {opt.label}
          </option>
        ))}
      </select>
      {hint && <p className="text-xs text-muted-foreground">{hint}</p>}
    </div>
  );
}

function FleetStats({
  models,
  health,
  cacheStats,
}: {
  models: ModelRoute[];
  health: ModelHealthSummary[];
  cacheStats?: CacheStats;
}) {
  const byModel = new Map(health.map((row) => [row.model_id, row]));
  const routes = models.filter((model) => !isBenchmarkRoute(model));
  const enabled = routes.filter((model) => model.enabled).length;
  let healthy = 0;
  let unhealthy = 0;
  for (const model of routes) {
    const status = byModel.get(model.id)?.status ?? "unknown";
    if (status === "healthy") healthy += 1;
    else if (status === "unhealthy") unhealthy += 1;
  }
  const unchecked = routes.length - healthy - unhealthy;
  const hits = cacheStats?.hits ?? 0;
  const misses = cacheStats?.misses ?? 0;
  const lookups = hits + misses;
  return (
    <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
      <StatCard label="Model routes" value={formatNumber(routes.length)} hint={`${formatNumber(enabled)} enabled`} />
      <StatCard
        label="Fleet health"
        value={routes.length === 0 ? "—" : `${formatNumber(healthy)} / ${formatNumber(routes.length)}`}
        hint={
          unhealthy > 0
            ? `${formatNumber(unhealthy)} unhealthy${unchecked > 0 ? ` / ${formatNumber(unchecked)} unchecked` : ""}`
            : unchecked > 0
              ? `${formatNumber(unchecked)} unchecked`
              : "all healthy"
        }
        tone={unhealthy > 0 ? "bad" : undefined}
      />
      <StatCard
        label="Cache hit rate"
        value={lookups > 0 ? `${((hits / lookups) * 100).toFixed(1)}%` : "—"}
        hint={`${formatNumber(lookups)} lookups / 24h`}
      />
      <StatCard label="Tokens saved" value={formatNumber(cacheStats?.tokens_saved ?? 0)} hint="response cache / 24h" />
    </div>
  );
}

function StatCard({ label, value, hint, tone }: { label: string; value: string; hint?: string; tone?: "bad" }) {
  return (
    <div className="rounded-lg border border-border bg-card p-4">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="mt-1 text-2xl font-semibold tabular-nums">{value}</p>
      {hint && (
        <p className={cn("mt-0.5 text-[11px]", tone === "bad" ? "text-destructive" : "text-muted-foreground")}>{hint}</p>
      )}
    </div>
  );
}

function fallbackHealth(model: ModelRoute): ModelHealthSummary {
  const now = new Date().toISOString();
  return {
    model_id: model.id,
    model_name: model.model_name,
    checks_enabled: true,
    alerts_enabled: true,
    check_interval_secs: 900,
    failure_threshold: 2,
    maintenance_until: null,
    maintenance_note: null,
    status: "unknown",
    consecutive_failures: 0,
    alert_state: "ok",
    next_check_at: now,
    last_checked_at: null,
    last_latency_ms: null,
    last_http_status: null,
    last_message: null,
    updated_at: now,
  };
}

function isBenchmarkRoute(model: ModelRoute) {
  const values = [model.model_name, model.upstream_model, model.api_base].join(" ").toLowerCase();
  return values.includes("benchmark-endpoint") || values.includes("mock-model") || values.includes("mock-backend");
}

// Editable, secret-free projection of a model route used for the JSON backup.
// `id`, timestamps and `api_key` are intentionally dropped: ids/timestamps are
// server-assigned and upstream secrets must never land in a downloaded file.
function toExportShape(model: ModelRoute) {
  return {
    model_name: model.model_name,
    description: model.description,
    upstream_model: model.upstream_model,
    api_base: model.api_base,
    model_type: model.model_type,
    input_cost_per_token: model.input_cost_per_token,
    output_cost_per_token: model.output_cost_per_token,
    cost_per_image: model.cost_per_image,
    cost_per_audio_second: model.cost_per_audio_second,
    cost_per_character: model.cost_per_character,
    context_window: model.context_window,
    admission_weight: model.admission_weight,
    max_in_flight: model.max_in_flight,
    supports_function_calling: model.supports_function_calling,
    supports_system_messages: model.supports_system_messages,
    supports_response_schema: model.supports_response_schema,
    supports_tool_choice: model.supports_tool_choice,
    supports_vision: model.supports_vision,
    enabled: model.enabled,
    cache_enabled: model.cache_enabled,
    cache_ttl_secs: model.cache_ttl_secs,
    tags: model.tags,
    boons: model.boons,
  };
}

function ImportPreview({
  plan,
  pending,
  onConfirm,
  onCancel,
}: {
  plan: ImportPlanItem[];
  pending: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const createCount = plan.filter((item) => item.action === "create").length;
  const updateCount = plan.length - createCount;
  return (
    <div className="rounded-md border border-border bg-card/40">
      <div className="flex flex-col gap-3 border-b border-border/60 px-4 py-3 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <p className="text-sm font-medium">Review import</p>
          <p className="text-xs text-muted-foreground">
            {plan.length} models in file / {createCount} new / {updateCount} to update
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button type="button" size="sm" variant="ghost" disabled={pending} onClick={onCancel}>
            Cancel
          </Button>
          <Button type="button" size="sm" disabled={pending} onClick={onConfirm}>
            {pending ? "Importing..." : `Confirm import (${plan.length})`}
          </Button>
        </div>
      </div>
      <div className="max-h-72 overflow-auto">
        <table className="w-full text-xs">
          <thead>
            <tr className="border-b border-border text-left text-muted-foreground">
              <th className="px-4 py-2 font-medium">Model</th>
              <th className="px-3 py-2 font-medium">Action</th>
              <th className="hidden px-3 py-2 font-medium md:table-cell">Upstream</th>
              <th className="px-3 py-2 font-medium">State</th>
            </tr>
          </thead>
          <tbody>
            {plan.map((item) => (
              <tr key={item.model_name} className="border-b border-border/50">
                <td className="px-4 py-2 font-medium">{item.model_name}</td>
                <td className="px-3 py-2">
                  <Badge
                    className={cn(
                      "text-[10px]",
                      item.action === "create"
                        ? "border-emerald-500/35 bg-emerald-500/10 text-emerald-300"
                        : "border-sky-500/35 bg-sky-500/10 text-sky-300",
                    )}
                  >
                    {item.action === "create" ? "new" : "update"}
                  </Badge>
                </td>
                <td className="hidden px-3 py-2 font-mono text-[11px] text-muted-foreground md:table-cell">{item.upstream_model}</td>
                <td className="px-3 py-2 text-muted-foreground">{item.enabled ? "enabled" : "disabled"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function ImportResultBanner({
  result,
  onDismiss,
}: {
  result: ImportModelsResult;
  onDismiss: () => void;
}) {
  if (!result.ok) {
    return (
      <div className="flex items-start justify-between gap-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
        <span>Import failed: {result.error}</span>
        <button type="button" onClick={onDismiss} className="shrink-0 text-xs underline opacity-80 hover:opacity-100">
          Dismiss
        </button>
      </div>
    );
  }
  const ok = result.failed === 0;
  return (
    <div
      className={cn(
        "rounded-md border px-3 py-2 text-sm",
        ok
          ? "border-emerald-500/35 bg-emerald-500/10 text-emerald-300"
          : "border-amber-500/35 bg-amber-500/10 text-amber-200",
      )}
    >
      <div className="flex items-start justify-between gap-3">
        <span>
          Imported {result.created} new, updated {result.updated}
          {result.failed > 0 ? `, ${result.failed} failed` : ""}.
        </span>
        <button type="button" onClick={onDismiss} className="shrink-0 text-xs underline opacity-80 hover:opacity-100">
          Dismiss
        </button>
      </div>
      {result.errors.length > 0 && (
        <ul className="mt-2 list-disc space-y-0.5 pl-5 text-xs">
          {result.errors.map((error, index) => (
            <li key={index}>{error}</li>
          ))}
        </ul>
      )}
    </div>
  );
}

function formatModelCost(model: ModelRoute) {
  const input = model.input_cost_per_token * 1_000_000;
  const output = model.output_cost_per_token * 1_000_000;
  if (input === 0 && output === 0) return "cost unset";
  return `in $${formatCostNumber(input)} / out $${formatCostNumber(output)} per 1M`;
}

function formatCostPerMillion(costPerToken: number) {
  if (!Number.isFinite(costPerToken) || costPerToken <= 0) return "Unset";
  return `$${formatCostNumber(costPerToken * 1_000_000)} / 1M tokens`;
}

function formatCostNumber(value: number) {
  if (value >= 1) return value.toFixed(2);
  if (value >= 0.01) return value.toFixed(3);
  return value.toPrecision(2);
}

// Renders a number as a plain decimal string, never scientific notation, so
// per-token costs like 0.00000008 stay editable in the form instead of showing
// as "8e-8". Expands the shortest round-trip representation by hand to avoid the
// float artifacts that `toFixed` introduces for tiny values.
function toPlainDecimal(value: number): string {
  if (!Number.isFinite(value)) return "";
  const str = String(value);
  if (!/e/i.test(str)) return str;

  const [coeff, expPart] = str.toLowerCase().split("e");
  const exp = Number(expPart);
  const negative = coeff.startsWith("-");
  const unsigned = coeff.replace("-", "");
  const digits = unsigned.replace(".", "");
  const dotIndex = unsigned.indexOf(".");
  const intLength = dotIndex === -1 ? unsigned.length : dotIndex;
  const pointPos = intLength + exp;

  let body: string;
  if (pointPos <= 0) {
    body = `0.${"0".repeat(-pointPos)}${digits}`;
  } else if (pointPos >= digits.length) {
    body = digits + "0".repeat(pointPos - digits.length);
  } else {
    body = `${digits.slice(0, pointPos)}.${digits.slice(pointPos)}`;
  }
  return (negative ? "-" : "") + body;
}

function capabilityList(model: ModelRoute): string[] {
  return [
    model.supports_function_calling && "functions",
    model.supports_system_messages && "system",
    model.supports_response_schema && "schema",
    model.supports_tool_choice && "tools",
    model.supports_vision && "vision",
  ].filter((value): value is string => Boolean(value));
}

function boonList(model: ModelRoute): string[] {
  return (model.boons ?? [])
    .map((value) => MODEL_BOONS.find((boon) => boon.value === value)?.label ?? value);
}

// Small read-only chip set for the overview spec strip.
function ChipList({ items, empty, tone }: { items: string[]; empty: string; tone?: "boon" }) {
  if (items.length === 0) {
    return <span className="text-xs font-normal text-muted-foreground">{empty}</span>;
  }
  return (
    <div className="flex max-w-[18rem] flex-wrap gap-1">
      {items.map((item) => (
        <Badge
          key={item}
          className={cn(
            "text-[10px] font-normal",
            tone === "boon"
              ? "border-amber-500/40 bg-amber-500/10 text-amber-600 dark:text-amber-400"
              : "border-border bg-background/60 text-muted-foreground",
          )}
        >
          {item}
        </Badge>
      ))}
    </div>
  );
}

function isInMaintenance(summary: ModelHealthSummary) {
  return summary.maintenance_until ? new Date(summary.maintenance_until).getTime() > Date.now() : false;
}

function formatTime(value: string) {
  return new Date(value).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

function middleTruncate(value: string, maxLength = 48): string {
  const trimmed = value.trim();
  if (trimmed.length <= maxLength) return trimmed;
  const head = Math.ceil((maxLength - 1) / 2);
  const tail = Math.floor((maxLength - 1) / 2);
  return `${trimmed.slice(0, head)}…${trimmed.slice(-tail)}`;
}

function datetimeLocalValue(value: string | null) {
  if (!value) return "";
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
