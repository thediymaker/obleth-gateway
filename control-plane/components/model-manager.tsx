"use client";

import { Fragment, useEffect, useMemo, useRef, useState, useTransition, type ChangeEvent, type ReactNode } from "react";
import {
  Activity,
  Check,
  ChevronDown,
  Database,
  Download,
  HeartPulse,
  PauseCircle,
  RefreshCw,
  Save,
  Trash2,
  Upload,
  XCircle,
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
  checkModelHealthAction,
  createModelAction,
  deleteModelAction,
  importModelsAction,
  planModelImportAction,
  setModelCacheAction,
  setModelCapacityAction,
  setModelHealthConfigAction,
  setModelWeightAction,
  updateModelAction,
  type ImportModelsResult,
  type ImportPlanItem,
} from "@/app/actions";
import { ChartShell, axisTick, chartGrid, compactAxis, tip, timeCursor } from "@/components/chart-tooltip";
import { Badge } from "@/components/ui/badge";
import { Button, buttonVariants } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import type { CacheStats, ModelHealthDetail, ModelHealthSummary, ModelRoute } from "@/lib/obleth";
import { cn, formatNumber } from "@/lib/utils";

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

export function ModelManager({
  models,
  cacheStats,
  health,
  healthDetails,
}: {
  models: ModelRoute[];
  cacheStats?: CacheStats;
  health: ModelHealthSummary[];
  healthDetails: Record<string, ModelHealthDetail | undefined>;
}) {
  const [pending, start] = useTransition();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [createType, setCreateType] = useState<string>("chat");
  const [showBenchmarkRoutes, setShowBenchmarkRoutes] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const createFormRef = useRef<HTMLFormElement>(null);
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
        createFormRef.current?.reset();
      } else {
        setCreateError(result.error);
      }
    });
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
      <CachePanel stats={cacheStats} />
      <Card>
        <CardHeader>
          <CardTitle>Add model route</CardTitle>
          <CardDescription>
            Map a client-facing model name to an upstream OpenAI-compatible endpoint.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form ref={createFormRef} action={submitModel} className="grid gap-4 md:grid-cols-2">
            <Field label="Model name (client)" name="model_name" placeholder="qwen3-vl-32b-instruct" required />
            <Field label="Upstream model" name="upstream_model" placeholder="asuair/qwen3-vl-32b-instruct" required />
            <div className="md:col-span-2">
              <SelectField
                label="Model type"
                name="model_type"
                value={createType}
                onChange={setCreateType}
                options={MODEL_TYPE_OPTIONS}
                hint={modelTypeHint(createType)}
              />
            </div>
            <div className="md:col-span-2">
              <Field label="Description" name="description" placeholder="Qwen3 235B instruction model for production chat and tool use" />
            </div>
            <div className="md:col-span-2">
              <Field label="API base URL" name="api_base" placeholder="http://envoy-aibrix-system.../v1" required />
            </div>
            <Field label="Upstream API key (optional)" name="api_key" placeholder="sk_..." />
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
            {createType === "chat" && (
              <>
                <div className="flex flex-wrap gap-4 md:col-span-2">
                  <Checkbox name="supports_function_calling" label="Function calling" />
                  <Checkbox name="supports_system_messages" label="System messages" defaultChecked />
                  <Checkbox name="supports_response_schema" label="Response schema" />
                  <Checkbox name="supports_tool_choice" label="Tool choice" />
                </div>
                <div className="md:col-span-2">
                  <Label className="mb-2 block">Routing tags (auto)</Label>
                  <div className="flex flex-wrap gap-3">
                    {MODEL_TAGS.map((tag) => (
                      <Checkbox key={tag} name={`tag_${tag}`} label={tag} />
                    ))}
                  </div>
                </div>
              </>
            )}
            <div className="md:col-span-2">
              <Button type="submit" disabled={pending}>{pending ? "Adding..." : "Add model"}</Button>
            </div>
            {createError && (
              <p className="md:col-span-2 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
                {createError}
              </p>
            )}
          </form>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div>
            <CardTitle>Configured models</CardTitle>
              <CardDescription>
                {visibleModels.length} visible / {models.length} registered
              {!showBenchmarkRoutes && benchmarkRouteCount > 0 ? ` / ${benchmarkRouteCount} benchmark hidden` : ""}
            </CardDescription>
          </div>
          <div className="flex items-center gap-2">
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
          <table className="w-full table-fixed text-sm">
            <thead>
              <tr className="border-b border-border text-left text-xs text-muted-foreground">
                <th className="w-[30%] px-6 py-3 font-medium">Model</th>
                <th className="w-[24%] px-3 py-3 font-medium">Health</th>
                <th className="hidden w-[28%] px-3 py-3 font-medium md:table-cell">Route</th>
                <th className="w-[46%] px-3 py-3 text-right font-medium md:w-[18%]" />
              </tr>
            </thead>
            <tbody>
              {visibleModels.map((model) => {
                const summary = healthByModel.get(model.id) ?? fallbackHealth(model);
                const selected = selectedId === model.id;
                return (
                  <Fragment key={model.id}>
                    <tr className="border-b border-border/60 align-top">
                      <td className="px-6 py-3">
                        <p className="font-medium">{model.model_name}</p>
                        {model.description && <p className="mt-1 line-clamp-2 text-xs text-muted-foreground">{model.description}</p>}
                        <div className="mt-2 flex flex-wrap gap-1.5">
                          {model.model_type && model.model_type !== "chat" && (
                            <Badge className="border-primary/40 bg-primary/15 text-[10px] text-primary">
                              {MODEL_TYPE_LABELS[model.model_type] ?? model.model_type}
                            </Badge>
                          )}
                          <Badge className="border-border bg-background text-[10px] text-muted-foreground">{formatModelCost(model)}</Badge>
                          <Badge className="border-border bg-background text-[10px] text-muted-foreground">{formatNumber(model.context_window)} ctx</Badge>
                          {model.tags?.map((tag) => (
                            <Badge key={tag} className="border-primary/30 bg-primary/10 text-[10px] text-primary">{tag}</Badge>
                          ))}
                        </div>
                        <p className="mt-1 truncate font-mono text-[11px] text-muted-foreground md:hidden">{model.upstream_model}</p>
                      </td>
                      <td className="px-3 py-3">
                        <HealthCell summary={summary} />
                      </td>
                      <td className="hidden px-3 py-3 md:table-cell">
                        <p className="truncate font-mono text-xs text-muted-foreground">{model.upstream_model}</p>
                        <p className="mt-0.5 truncate font-mono text-[11px] text-muted-foreground/70">{model.api_base}</p>
                      </td>
                      <td className="px-3 py-3">
                        <div className="flex items-center justify-end gap-1.5">
                          <Button type="button" size="icon" variant="secondary" disabled={pending} onClick={() => checkOne(model.id)} title="Check health">
                            <Activity className="h-3.5 w-3.5" />
                          </Button>
                          <Button
                            type="button"
                            size="icon"
                            variant="ghost"
                            aria-expanded={selected}
                            title={selected ? "Collapse details" : "Expand details"}
                            onClick={() => setSelectedId((current) => (current === model.id ? null : model.id))}
                          >
                            <ChevronDown className={cn("h-3.5 w-3.5 transition-transform", selected && "rotate-180")} />
                          </Button>
                          <Button
                            variant="ghost"
                            size="sm"
                            className="text-destructive hover:text-destructive"
                            disabled={pending}
                            onClick={() => removeModel(model)}
                          >
                            <Trash2 className="h-3.5 w-3.5" />
                          </Button>
                        </div>
                      </td>
                    </tr>
                    {selected && (
                      <tr className="border-b border-border/60">
                        <td colSpan={4} className="bg-background/35 px-6 py-5">
                          <ModelDetailPanel
                            model={model}
                            summary={summary}
                            detail={healthDetails[model.id]}
                            pending={pending}
                            onCacheToggle={() => start(() => setModelCacheAction(model.id, !model.cache_enabled, model.cache_ttl_secs || 300))}
                          />
                        </td>
                      </tr>
                    )}
                  </Fragment>
                );
              })}
              {visibleModels.length === 0 && (
                <tr>
                  <td colSpan={4} className="px-6 py-8 text-center text-muted-foreground">
                    {models.length === 0 ? "No models configured." : "Only benchmark endpoints are hidden."}
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </CardContent>
      </Card>
    </div>
  );
}

function ModelDetailPanel({
  model,
  summary,
  detail,
  pending,
  onCacheToggle,
}: {
  model: ModelRoute;
  summary: ModelHealthSummary;
  detail?: ModelHealthDetail;
  pending: boolean;
  onCacheToggle: () => void;
}) {
  const [editType, setEditType] = useState<string>(model.model_type || "chat");
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
    <div className="grid gap-5 xl:grid-cols-[minmax(0,1.1fr)_minmax(320px,0.9fr)]">
      <div className="space-y-5">
        <div className="rounded-md border border-border bg-card/40 p-4">
          <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
            <div className="min-w-0">
              <p className="text-sm font-medium">Model metadata</p>
              <p className="mt-1 text-sm text-muted-foreground">
                {model.description || "No description set."}
              </p>
            </div>
            <div className="shrink-0">
              <HealthBadge summary={summary} />
            </div>
          </div>
          <div className="mt-4 grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
            <MiniStat label="Input" value={formatCostPerMillion(model.input_cost_per_token)} />
            <MiniStat label="Output" value={formatCostPerMillion(model.output_cost_per_token)} />
            <MiniStat label="Context" value={formatNumber(model.context_window)} />
            <MiniStat label="Features" value={formatCapabilities(model)} />
          </div>
        </div>

        <div className="rounded-md border border-border bg-card/40 p-4">
          <p className="text-sm font-medium">Operational controls</p>
          <div className="mt-4 grid grid-cols-[repeat(auto-fit,minmax(170px,1fr))] gap-3">
            <ControlBlock label="Route status">
              <Badge className={model.enabled ? "text-foreground" : "opacity-50"}>{model.enabled ? "enabled" : "disabled"}</Badge>
            </ControlBlock>
            <ControlBlock label="Response cache">
              <CacheToggle
                enabled={model.cache_enabled}
                ttlSecs={model.cache_ttl_secs || 300}
                disabled={pending}
                onToggle={onCacheToggle}
              />
            </ControlBlock>
            <ControlBlock label="Admission weight">
              <ModelWeightControl id={model.id} initial={model.admission_weight} />
            </ControlBlock>
            <ControlBlock label="Max slots">
              <ModelCapacityControl id={model.id} initial={model.max_in_flight} />
            </ControlBlock>
          </div>
        </div>

        <div className="rounded-md border border-border bg-card/40 p-4">
          <div className="mb-4 flex items-center justify-between gap-3">
            <div>
              <p className="text-sm font-medium">Route settings</p>
              <p className="text-xs text-muted-foreground">Client name stays fixed; update upstream routing and capabilities here.</p>
            </div>
          </div>
          <form action={updateModelAction} className="grid gap-3 md:grid-cols-2">
            <input type="hidden" name="id" value={model.id} />
            <Field label="Upstream model" name="upstream_model" defaultValue={model.upstream_model} required />
            <Field label="API base URL" name="api_base" defaultValue={model.api_base} required />
            <div className="md:col-span-2">
              <Field label="Description" name="description" defaultValue={model.description} />
            </div>
            <div className="md:col-span-2">
              <SelectField
                label="Model type"
                name="model_type"
                value={editType}
                onChange={setEditType}
                options={MODEL_TYPE_OPTIONS}
                hint={modelTypeHint(editType)}
              />
            </div>
            <Field label="Upstream API key" name="api_key" placeholder="Leave blank to keep current" />
            <Field label="Admission weight" name="admission_weight" type="number" defaultValue={String(model.admission_weight)} />
            <Field label="Max in-flight" name="max_in_flight" type="number" defaultValue={model.max_in_flight == null ? "" : String(model.max_in_flight)} />
            {(editType === "chat" || editType === "embedding") && (
              <Field label="Context window" name="context_window" type="number" defaultValue={String(model.context_window)} />
            )}
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
            {editType === "chat" ? (
              <>
                <div className="flex flex-wrap gap-4 md:col-span-2">
                  <Checkbox name="enabled" label="Enabled" defaultChecked={model.enabled} />
                  <Checkbox name="supports_function_calling" label="Function calling" defaultChecked={model.supports_function_calling} />
                  <Checkbox name="supports_system_messages" label="System messages" defaultChecked={model.supports_system_messages} />
                  <Checkbox name="supports_response_schema" label="Response schema" defaultChecked={model.supports_response_schema} />
                  <Checkbox name="supports_tool_choice" label="Tool choice" defaultChecked={model.supports_tool_choice} />
                </div>
                <div className="md:col-span-2">
                  <Label className="mb-2 block">Routing tags (auto)</Label>
                  <div className="flex flex-wrap gap-3">
                    {MODEL_TAGS.map((tag) => (
                      <Checkbox
                        key={tag}
                        name={`tag_${tag}`}
                        label={tag}
                        defaultChecked={model.tags?.includes(tag) ?? false}
                      />
                    ))}
                  </div>
                </div>
              </>
            ) : (
              <div className="flex flex-wrap gap-4 md:col-span-2">
                <Checkbox name="enabled" label="Enabled" defaultChecked={model.enabled} />
              </div>
            )}
            <div className="md:col-span-2">
              <Button type="submit" size="sm" disabled={pending}>
                <Save className="h-3.5 w-3.5" />
                Save route
              </Button>
            </div>
          </form>
        </div>

        <div className="rounded-md border border-border bg-card/40 p-4">
          <p className="text-sm font-medium">Health config</p>
          <form action={setModelHealthConfigAction} className="mt-4 grid gap-3 md:grid-cols-2">
            <input type="hidden" name="id" value={model.id} />
            <Field label="Interval seconds" name="check_interval_secs" type="number" defaultValue={String(summary.check_interval_secs)} />
            <Field label="Failure threshold" name="failure_threshold" type="number" defaultValue={String(summary.failure_threshold)} />
            <Field label="Maintenance until" name="maintenance_until" type="datetime-local" defaultValue={datetimeLocalValue(summary.maintenance_until)} />
            <Field label="Maintenance note" name="maintenance_note" defaultValue={summary.maintenance_note ?? ""} />
            <div className="flex flex-wrap gap-4 md:col-span-2">
              <Checkbox name="checks_enabled" label="Scheduled checks" defaultChecked={summary.checks_enabled} />
              <Checkbox name="alerts_enabled" label="Slack alerts" defaultChecked={summary.alerts_enabled} />
            </div>
            <div className="md:col-span-2">
              <Button type="submit" size="sm" disabled={pending}>
                <Save className="h-3.5 w-3.5" />
                Save health
              </Button>
            </div>
          </form>
        </div>
      </div>

      <div className="space-y-5">
        <div className="rounded-md border border-border bg-card/40 p-4">
          <div className="mb-3 flex items-center justify-between gap-3">
            <div>
              <p className="text-sm font-medium">Health trend</p>
              <p className="text-xs text-muted-foreground">Recent health probe latency</p>
            </div>
            <HealthBadge summary={summary} />
          </div>
          {chartData.length === 0 ? (
            <div className="flex h-48 items-center justify-center rounded-sm border border-border/70 text-xs text-muted-foreground">No checks yet</div>
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
        </div>

        <div className="rounded-md border border-border bg-card/40 p-4">
          <p className="text-sm font-medium">Recent checks</p>
          <div className="mt-3 max-h-80 overflow-auto">
            <table className="w-full text-xs">
              <thead>
                <tr className="border-b border-border text-left text-muted-foreground">
                  <th className="py-2 pr-3 font-medium">Time</th>
                  <th className="py-2 pr-3 font-medium">Status</th>
                  <th className="py-2 pr-3 font-medium">HTTP</th>
                  <th className="py-2 pr-3 font-medium">Latency</th>
                </tr>
              </thead>
              <tbody>
                {checks.map((check) => (
                  <tr key={check.id} className="border-b border-border/50">
                    <td className="py-2 pr-3 tabular-nums text-muted-foreground">{formatTime(check.checked_at)}</td>
                    <td className="py-2 pr-3"><StatusPill status={check.status} /></td>
                    <td className="py-2 pr-3 tabular-nums text-muted-foreground">{check.http_status ?? "-"}</td>
                    <td className="py-2 pr-3 tabular-nums text-muted-foreground">{check.latency_ms == null ? "-" : `${formatNumber(check.latency_ms)} ms`}</td>
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
          {summary.last_message && <p className="mt-3 rounded-sm border border-border/70 bg-background/40 p-2 text-xs text-muted-foreground">{summary.last_message}</p>}
        </div>
      </div>
    </div>
  );
}

export function ModelCapacityControl({ id, initial }: { id: string; initial: number | null }) {
  const [value, setValue] = useState(initial == null ? "" : String(initial));
  const [pending, start] = useTransition();
  useEffect(() => {
    setValue(initial == null ? "" : String(initial));
  }, [initial]);
  const next = value.trim() === "" ? null : Math.max(1, Math.round(Number(value)) || 1);
  const changed = next !== initial;

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
      <Button type="button" size="sm" variant="secondary" disabled={pending || !changed} onClick={() => start(() => setModelCapacityAction(id, next))}>
        {pending ? "Saving" : "Apply"}
      </Button>
    </div>
  );
}

export function ModelWeightControl({ id, initial }: { id: string; initial: number }) {
  const [value, setValue] = useState(String(initial));
  const [pending, start] = useTransition();
  useEffect(() => {
    setValue(String(initial));
  }, [initial]);
  const next = Math.max(1, Math.round(Number(value)) || 1);
  const changed = next !== initial;

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
      <Button type="button" size="sm" variant="secondary" disabled={pending || !changed} onClick={() => start(() => setModelWeightAction(id, next))}>
        {pending ? "Saving" : "Apply"}
      </Button>
    </div>
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
  if (status === "maintenance") return "border-amber-500/35 bg-amber-500/10 text-amber-300";
  if (status === "disabled") return "border-border bg-muted/30 text-muted-foreground";
  return "border-border bg-background text-muted-foreground";
}

function ControlBlock({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="min-w-0 rounded-sm border border-border/70 bg-background/35 p-3">
      <p className="mb-2 text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      {children}
    </div>
  );
}

function MiniStat({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 rounded-sm border border-border/70 bg-background/35 p-3">
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      <p className="mt-1 truncate text-sm font-medium tabular-nums">{value}</p>
    </div>
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

function CachePanel({ stats }: { stats?: CacheStats }) {
  const hits = stats?.hits ?? 0;
  const misses = stats?.misses ?? 0;
  const lookups = hits + misses;
  const hitRate = lookups > 0 ? (hits / lookups) * 100 : 0;
  return (
    <Card>
      <CardHeader>
        <CardTitle>Response cache</CardTitle>
        <CardDescription>Exact-match cache offload over the last 24h.</CardDescription>
      </CardHeader>
      <CardContent className="grid grid-cols-2 gap-4 md:grid-cols-4">
        <Stat label="Hit rate" value={`${hitRate.toFixed(1)}%`} />
        <Stat label="Cache hits" value={formatNumber(hits)} />
        <Stat label="Misses" value={formatNumber(misses)} />
        <Stat label="Tokens saved" value={formatNumber(stats?.tokens_saved ?? 0)} />
      </CardContent>
    </Card>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="text-2xl font-semibold tabular-nums">{value}</div>
    </div>
  );
}

function Checkbox({ name, label, defaultChecked }: { name: string; label: string; defaultChecked?: boolean }) {
  return (
    <label className="flex items-center gap-2 text-xs text-muted-foreground">
      <input type="checkbox" name={name} defaultChecked={defaultChecked} className="rounded border-border" />
      {label}
    </label>
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
    enabled: model.enabled,
    cache_enabled: model.cache_enabled,
    cache_ttl_secs: model.cache_ttl_secs,
    tags: model.tags,
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

function formatCapabilities(model: ModelRoute) {
  const capabilities = [
    model.supports_function_calling && "functions",
    model.supports_system_messages && "system",
    model.supports_response_schema && "schema",
    model.supports_tool_choice && "tools",
  ].filter(Boolean);
  return capabilities.length > 0 ? capabilities.join(", ") : "None";
}

function isInMaintenance(summary: ModelHealthSummary) {
  return summary.maintenance_until ? new Date(summary.maintenance_until).getTime() > Date.now() : false;
}

function formatTime(value: string) {
  return new Date(value).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

function datetimeLocalValue(value: string | null) {
  if (!value) return "";
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
