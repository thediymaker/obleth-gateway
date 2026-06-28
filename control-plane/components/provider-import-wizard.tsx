// control-plane/components/provider-import-wizard.tsx
"use client";

import { useMemo, useState, useTransition } from "react";
import { Check, Search } from "lucide-react";
import {
  importModelsAction,
  listUpstreamModelsAction,
  planModelImportAction,
  type ImportModelsResult,
  type ImportPlanItem,
} from "@/app/actions";
import { ImportPreview, ImportResultBanner } from "@/components/model-manager";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import type { ModelRoute } from "@/lib/obleth";
import { normalizeModelApiNameFinal } from "@/lib/model-name";
import {
  buildImportPayload,
  classifyDiscovered,
  type BatchDefaults,
  type DiscoveredRow,
  type RowState,
} from "@/lib/provider-import";
import { cn } from "@/lib/utils";

const MODEL_TYPE_OPTIONS = [
  { value: "chat", label: "Chat / completions" },
  { value: "embedding", label: "Embeddings" },
  { value: "audio_transcription", label: "Audio transcription (STT)" },
  { value: "audio_speech", label: "Text to speech (TTS)" },
  { value: "image", label: "Image generation" },
] as const;

type Step = "connect" | "defaults" | "select" | "review";

export function ProviderImportWizard({
  models,
  onClose,
}: {
  models: ModelRoute[];
  onClose: () => void;
}) {
  const [step, setStep] = useState<Step>("connect");
  const [pending, start] = useTransition();

  // Connect
  const [apiBase, setApiBase] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [base, setBase] = useState("");
  const [error, setError] = useState<string | null>(null);

  // Defaults
  const [defaults, setDefaults] = useState<BatchDefaults>({
    model_type: "chat",
    context_window: 8192,
    input_cost_per_token: 0,
    output_cost_per_token: 0,
    enabled: true,
  });

  // Select
  const [discovered, setDiscovered] = useState<DiscoveredRow[]>([]);
  const [rows, setRows] = useState<Record<string, RowState>>({});
  const [filter, setFilter] = useState("");
  const [showExisting, setShowExisting] = useState(false);

  // Review
  const [plan, setPlan] = useState<ImportPlanItem[] | null>(null);
  const [result, setResult] = useState<ImportModelsResult | null>(null);

  const newRows = discovered.filter((d) => d.status === "new");
  const existingCount = discovered.length - newRows.length;
  const visible = useMemo(() => {
    const q = filter.trim().toLowerCase();
    const pool = showExisting ? discovered : newRows;
    if (!q) return pool;
    return pool.filter((d) => d.id.toLowerCase().includes(q) || d.modelName.includes(q));
  }, [discovered, newRows, filter, showExisting]);

  function fetchModels() {
    setError(null);
    start(async () => {
      const res = await listUpstreamModelsAction({ apiBase, apiKey: apiKey || undefined });
      if (!res.ok) {
        setError(res.error);
        return;
      }
      const classified = classifyDiscovered(
        res.models,
        models.map((m) => ({
          model_name: m.model_name,
          upstream_model: m.upstream_model,
          api_base: m.api_base,
        })),
        res.base,
      );
      setBase(res.base);
      setDiscovered(classified);
      const initial: Record<string, RowState> = {};
      for (const d of classified) {
        initial[d.id] = {
          id: d.id,
          modelName: d.modelName,
          included: d.status === "new",
          overrides: {},
        };
      }
      setRows(initial);
      setStep("defaults");
    });
  }

  function setRow(id: string, patch: Partial<RowState>) {
    setRows((cur) => ({ ...cur, [id]: { ...cur[id], ...patch } }));
  }

  function setRowOverride(id: string, patch: Partial<BatchDefaults>) {
    setRows((cur) => ({ ...cur, [id]: { ...cur[id], overrides: { ...cur[id].overrides, ...patch } } }));
  }

  function toggleAllVisible(on: boolean) {
    setRows((cur) => {
      const next = { ...cur };
      for (const d of visible) {
        if (d.status === "new") next[d.id] = { ...next[d.id], included: on };
      }
      return next;
    });
  }

  const selectedRows = newRows.map((d) => rows[d.id]).filter((r) => r?.included);

  function goReview() {
    setError(null);
    const bad = selectedRows.find((r) => !normalizeModelApiNameFinal(r.modelName) || !r.id);
    if (bad) {
      setError("Every selected model needs a name.");
      return;
    }
    if (selectedRows.length === 0) {
      setError("Select at least one model to import.");
      return;
    }
    const payload = buildImportPayload(Object.values(rows), base, apiKey || undefined, defaults);
    start(async () => {
      const res = await planModelImportAction(JSON.stringify(payload));
      if (!res.ok) {
        setError(res.error);
        return;
      }
      setPlan(res.plan);
      setStep("review");
    });
  }

  function confirmImport() {
    const payload = buildImportPayload(Object.values(rows), base, apiKey || undefined, defaults);
    start(async () => {
      const res = await importModelsAction(JSON.stringify(payload));
      setResult(res);
      setPlan(null);
    });
  }

  return (
    <Card className="overflow-hidden border-primary/25 bg-card/80">
      <CardHeader className="flex-row items-start justify-between gap-3 border-b border-border/70 bg-background/30">
        <div>
          <CardTitle>Import from provider</CardTitle>
          <CardDescription>
            Discover an OpenAI-compatible provider&apos;s models and import the ones you don&apos;t have yet.
          </CardDescription>
        </div>
        <Button type="button" size="sm" variant="ghost" disabled={pending} onClick={onClose}>
          Cancel
        </Button>
      </CardHeader>

      <CardContent className="space-y-5 p-5 sm:p-6">
        {error && (
          <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
            {error}
          </p>
        )}

        {result && <ImportResultBanner result={result} onDismiss={onClose} />}

        {!result && step === "connect" && (
          <section className="grid gap-4 md:max-w-xl">
            <div className="space-y-1.5">
              <Label htmlFor="prov-base">API base URL</Label>
              <Input
                id="prov-base"
                value={apiBase}
                onChange={(e) => setApiBase(e.target.value)}
                placeholder="https://openrouter.ai/api/v1"
                className="font-mono"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="prov-key">API key (optional)</Label>
              <Input
                id="prov-key"
                type="password"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder="sk-..."
                className="font-mono"
              />
            </div>
            <div>
              <Button type="button" disabled={pending || !apiBase.trim()} onClick={fetchModels}>
                {pending ? "Fetching..." : "Fetch models"}
              </Button>
            </div>
          </section>
        )}

        {!result && step === "defaults" && (
          <section className="space-y-4">
            <p className="text-sm text-muted-foreground">
              Discovered {discovered.length} models ({newRows.length} new, {existingCount} already imported).
              Set defaults applied to every imported model; override per row next.
            </p>
            <div className="grid gap-4 md:grid-cols-2 md:max-w-2xl">
              <div className="space-y-1.5">
                <Label>Default type</Label>
                <Select
                  value={defaults.model_type}
                  onChange={(e) => setDefaults((d) => ({ ...d, model_type: e.target.value }))}
                >
                  {MODEL_TYPE_OPTIONS.map((o) => (
                    <option key={o.value} value={o.value}>
                      {o.label}
                    </option>
                  ))}
                </Select>
              </div>
              <NumberField
                label="Context window"
                value={defaults.context_window}
                onChange={(v) => setDefaults((d) => ({ ...d, context_window: v }))}
              />
              <NumberField
                label="Input cost / token"
                value={defaults.input_cost_per_token}
                onChange={(v) => setDefaults((d) => ({ ...d, input_cost_per_token: v }))}
              />
              <NumberField
                label="Output cost / token"
                value={defaults.output_cost_per_token}
                onChange={(v) => setDefaults((d) => ({ ...d, output_cost_per_token: v }))}
              />
            </div>
            <div className="flex justify-between">
              <Button type="button" variant="outline" onClick={() => setStep("connect")}>
                Back
              </Button>
              <Button type="button" onClick={() => setStep("select")}>
                Next
              </Button>
            </div>
          </section>
        )}

        {!result && step === "select" && (
          <section className="space-y-3">
            <div className="flex flex-wrap items-center gap-2">
              <div className="relative flex-1 min-w-48">
                <Search className="absolute left-2.5 top-2.5 h-3.5 w-3.5 text-muted-foreground" />
                <Input
                  value={filter}
                  onChange={(e) => setFilter(e.target.value)}
                  placeholder="Filter models"
                  className="pl-8"
                />
              </div>
              <Button type="button" size="sm" variant="outline" onClick={() => toggleAllVisible(true)}>
                Select all
              </Button>
              <Button type="button" size="sm" variant="outline" onClick={() => toggleAllVisible(false)}>
                Clear
              </Button>
              {existingCount > 0 && (
                <Button type="button" size="sm" variant="ghost" onClick={() => setShowExisting((s) => !s)}>
                  {showExisting ? "Hide" : "Show"} {existingCount} already imported
                </Button>
              )}
            </div>

            <div className="max-h-96 overflow-auto rounded-md border border-border/70">
              {visible.map((d) => {
                const r = rows[d.id];
                const isExisting = d.status === "existing";
                return (
                  <div
                    key={d.id}
                    className={cn(
                      "flex items-center gap-3 border-b border-border/40 px-3 py-2 last:border-b-0",
                      isExisting && "opacity-60",
                    )}
                  >
                    <input
                      type="checkbox"
                      disabled={isExisting}
                      checked={!isExisting && !!r?.included}
                      onChange={(e) => setRow(d.id, { included: e.target.checked })}
                    />
                    <div className="min-w-0 flex-1">
                      <p className="truncate font-mono text-xs" title={d.id}>
                        {d.id}
                      </p>
                      {!isExisting ? (
                        <Input
                          value={r?.modelName ?? ""}
                          onChange={(e) => setRow(d.id, { modelName: e.target.value })}
                          onBlur={(e) => setRow(d.id, { modelName: normalizeModelApiNameFinal(e.target.value) })}
                          className="mt-1 h-7 font-mono text-xs lowercase"
                        />
                      ) : (
                        <p className="mt-0.5 text-[11px] text-muted-foreground">→ {d.modelName}</p>
                      )}
                    </div>
                    {!isExisting && (
                      <Select
                        value={r?.overrides.model_type ?? defaults.model_type}
                        onChange={(e) => setRowOverride(d.id, { model_type: e.target.value })}
                        className="h-7 w-44 text-xs"
                      >
                        {MODEL_TYPE_OPTIONS.map((o) => (
                          <option key={o.value} value={o.value}>
                            {o.label}
                          </option>
                        ))}
                      </Select>
                    )}
                    {isExisting && <Badge className="bg-background text-[10px]">Already imported</Badge>}
                  </div>
                );
              })}
              {visible.length === 0 && (
                <div className="px-3 py-8 text-center text-sm text-muted-foreground">
                  {newRows.length === 0 ? "No new models — you already have them all." : "No models match the filter."}
                </div>
              )}
            </div>

            <div className="flex items-center justify-between">
              <Button type="button" variant="outline" onClick={() => setStep("defaults")}>
                Back
              </Button>
              <div className="flex items-center gap-3">
                <span className="text-xs text-muted-foreground">{selectedRows.length} selected</span>
                <Button type="button" disabled={pending} onClick={goReview}>
                  Review import
                </Button>
              </div>
            </div>
          </section>
        )}

        {!result && step === "review" && plan && (
          <section className="space-y-3">
            <ImportPreview plan={plan} pending={pending} onConfirm={confirmImport} onCancel={() => setStep("select")} />
          </section>
        )}
      </CardContent>
    </Card>
  );
}

function NumberField({
  label,
  value,
  onChange,
}: {
  label: string;
  value: number | undefined;
  onChange: (v: number | undefined) => void;
}) {
  return (
    <div className="space-y-1.5">
      <Label>{label}</Label>
      <Input
        type="number"
        value={value ?? ""}
        onChange={(e) => onChange(e.target.value === "" ? undefined : Number(e.target.value))}
      />
    </div>
  );
}
