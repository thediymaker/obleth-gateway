"use client";

import { useEffect, useState, useTransition } from "react";
import { useQuery } from "@tanstack/react-query";
import { RefreshCw, Zap } from "lucide-react";
import { applyAutotuneCapacityAction, autotuneModelAction } from "@/app/actions";
import { HelpTip } from "@/components/fairshare/help";
import { useCapacityDiscovery } from "@/components/fairshare/hooks";
import { Field, TextField } from "@/components/models/fields";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, DialogTrigger } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import type {
  AutotuneReport,
  AutotuneWorkload,
  CapacityDiscoveryModelView,
  CapacityDiscoveryView,
  CapacityServiceSummary,
  CapacityServicesView,
  FairshareSlotMode,
  ModelRoute,
} from "@/lib/obleth";
import { cn, getJson } from "@/lib/utils";

const CAPACITY_MODES = ["static", "tuned", "discovered"] as const;
export type CapacityMode = (typeof CAPACITY_MODES)[number];

export function capacityMode(mode: string): CapacityMode {
  return (CAPACITY_MODES as readonly string[]).includes(mode) ? (mode as CapacityMode) : "static";
}

/** Static, tuned or discovered, submitted as `capacity_mode` and saved with the page. */
export function CapacityModeToggle({ mode, onChange }: { mode: CapacityMode; onChange: (mode: CapacityMode) => void }) {
  return (
    <div className="inline-flex gap-0.5 rounded-[9px] border border-border p-[3px]" role="group" aria-label="Capacity mode">
      <input type="hidden" name="capacity_mode" value={mode} />
      {CAPACITY_MODES.map((opt) => (
        <button
          key={opt}
          type="button"
          aria-pressed={mode === opt}
          onClick={() => onChange(opt)}
          className={cn(
            "inline-flex h-7 items-center rounded-md px-3 text-[12.5px] capitalize transition-colors",
            mode === opt ? "bg-secondary text-foreground" : "text-muted-foreground hover:text-foreground",
          )}
        >
          {opt}
        </button>
      ))}
    </div>
  );
}

/** The live derivation in one line: ready × per replica (× headroom) = limit. */
export function discoveryEquation(status?: CapacityDiscoveryView["models"][number]["status"]): string | null {
  if (!status) return null;
  const ready = status.ready_replicas ?? "—";
  const headroom = status.headroom !== 1 ? ` × ${status.headroom}` : "";
  if (status.per_replica_max_in_flight == null && status.replica_capacity != null) {
    // Endpoints with values of their own: summed, not multiplied.
    const sum = headroom ? `(${status.replica_capacity} summed)` : `${status.replica_capacity} summed`;
    return `${ready} ready, ${sum}${headroom} = ${status.effective_max_in_flight}`;
  }
  const per = status.per_replica_max_in_flight ?? "—";
  return `${ready} ready × ${per} per replica${headroom} = ${status.effective_max_in_flight}`;
}

function serviceLine(s: CapacityServiceSummary): string {
  return `${s.service} · ${s.namespace} · ${s.ready} ready`;
}

/**
 * Chooses the Service whose ready endpoints are the model's replicas, from
 * the Services the gateway can see. Shows the model's default Service as a
 * confirmed line when one matches, otherwise (or on "Change") a searchable
 * list. Choosing one sets the Service and its namespace together; "Use
 * default" clears both. The choice rides the surrounding form as hidden
 * `capacity_service` / `capacity_namespace` fields.
 */
export function CapacityServicePicker({
  model,
  status,
}: {
  model: ModelRoute;
  status?: CapacityDiscoveryView["models"][number]["status"];
}) {
  const [service, setService] = useState(model.capacity_service ?? "");
  const [namespace, setNamespace] = useState(model.capacity_namespace ?? "");
  const [choosing, setChoosing] = useState(false);
  const [search, setSearch] = useState("");
  useEffect(() => {
    setService(model.capacity_service ?? "");
    setNamespace(model.capacity_namespace ?? "");
  }, [model.capacity_service, model.capacity_namespace]);
  const { data, isError } = useQuery({
    queryKey: ["capacity-services", model.model_name],
    queryFn: () => getJson<CapacityServicesView>(`/api/live/capacity/services?model=${encodeURIComponent(model.model_name)}`),
    staleTime: 15_000,
  });
  const services = data?.services ?? [];
  const chosen: CapacityServiceSummary | null = service
    ? (services.find((s) => s.service === service && (!namespace || s.namespace === namespace)) ?? {
        service,
        namespace: namespace || "an allowed namespace",
        ready: -1,
      })
    : (data?.default_match ?? null);
  const showPicker = choosing || !chosen;
  const q = search.trim().toLowerCase();
  const filtered = q ? services.filter((s) => s.service.toLowerCase().includes(q) || s.namespace.toLowerCase().includes(q)) : services;
  const choose = (next: CapacityServiceSummary | null) => {
    setService(next?.service ?? "");
    setNamespace(next?.namespace ?? "");
    setChoosing(false);
    setSearch("");
  };
  const equation = discoveryEquation(status);

  return (
    <div className="space-y-1.5" data-testid="service-picker">
      <p className="text-[12.5px] font-medium text-secondary-foreground">Service</p>
      <input type="hidden" name="capacity_service" value={service} />
      <input type="hidden" name="capacity_namespace" value={namespace} />
      {chosen && !showPicker && (
        <p className="text-xs" data-testid="service-chosen">
          Using <code className="rounded bg-muted/50 px-1">{chosen.service}</code> in <code className="rounded bg-muted/50 px-1">{chosen.namespace}</code>
          {chosen.ready >= 0 ? ` · ${chosen.ready} ready` : " · not listed right now"}
          {!service && <span className="text-muted-foreground"> (default)</span>}{" "}
          <button type="button" className="underline underline-offset-2 hover:text-foreground" onClick={() => setChoosing(true)}>
            Change
          </button>
        </p>
      )}
      {showPicker && (
        <div className="rounded-md border border-border">
          <Input
            aria-label="Search Services"
            placeholder="Search Services"
            className="h-8 rounded-b-none border-0 border-b border-border text-xs"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
          <div role="listbox" aria-label="Services" className="max-h-48 overflow-y-auto py-1 text-xs">
            <button type="button" role="option" aria-selected={!service} className="block w-full px-2 py-1 text-left hover:bg-muted/50" onClick={() => choose(null)}>
              Use default
              {data?.default_service && <span className="text-muted-foreground"> · {data.default_service}</span>}
            </button>
            {filtered.map((s) => (
              <button
                key={`${s.namespace}/${s.service}`}
                type="button"
                role="option"
                aria-selected={s.service === service && s.namespace === namespace}
                className="block w-full px-2 py-1 text-left tabular-nums hover:bg-muted/50"
                onClick={() => choose(s)}
              >
                {serviceLine(s)}
              </button>
            ))}
            {data && filtered.length === 0 && (
              <p className="px-2 py-1 text-muted-foreground">{services.length === 0 ? (data.reason ?? "No Services found.") : "No Service matches the search."}</p>
            )}
            {isError && <p className="px-2 py-1 text-foreground">Could not list the Services.</p>}
          </div>
        </div>
      )}
      {!service && data?.default_service && !data.default_match && (
        <p className="text-xs text-foreground">No Service named {data.default_service} was found in the allowed namespaces; choose one above.</p>
      )}
      {data?.errors.map((e) => (
        <p key={e} className="text-xs text-foreground">{e}</p>
      ))}
      <p className="text-xs text-muted-foreground">For multi-node serving, pick a Service that selects only the pods that take requests.</p>
      {equation && (
        <p className="text-xs tabular-nums text-muted-foreground" data-testid="discovery-equation">
          Live: {equation}
        </p>
      )}
    </div>
  );
}

/**
 * The discovered mode's settings, submitted with the page, and what the
 * answering gateway replica derived from them: ready replicas × per-replica
 * concurrency × headroom.
 */
export function CapacityDiscoveryFields({ model }: { model: ModelRoute }) {
  const [source, setSource] = useState(model.capacity_source ?? "endpoints");
  useEffect(() => {
    setSource(model.capacity_source ?? "endpoints");
  }, [model.capacity_source]);
  const { data: view } = useCapacityDiscovery();
  const entry = view?.models.find((m) => m.model_id === model.id);
  const status = entry?.status;

  return (
    <div className="grid gap-3" data-testid="capacity-discovery">
      <div className="grid gap-3 md:grid-cols-2">
        <div className="space-y-1.5">
          <p className="text-[12.5px] font-medium text-secondary-foreground">Capacity source</p>
          <Select
            name="capacity_source"
            aria-label="Capacity source"
            value={source}
            onValueChange={setSource}
            options={[
              { value: "endpoints", label: "endpoints", hint: "This model's enabled, healthy endpoints" },
              { value: "kubernetes", label: "kubernetes", hint: "Ready endpoints of a Kubernetes Service" },
            ]}
          />
        </div>
        <Field
          label={source === "kubernetes" ? "Per-replica concurrency (required)" : "Per-replica concurrency"}
          name="per_replica_max_in_flight"
          type="number"
          min={1}
          required={source === "kubernetes"}
          placeholder={source === "kubernetes" ? "e.g. 8" : "blank: each endpoint's own max in flight"}
          defaultValue={model.per_replica_max_in_flight == null ? "" : String(model.per_replica_max_in_flight)}
          hint="Requests one replica serves at once, e.g. your server's max concurrent sequences, such as vLLM --max-num-seqs."
        />
        {source === "kubernetes" && (
          <div className="md:col-span-2">
            <CapacityServicePicker model={model} status={status} />
          </div>
        )}
        <Field
          label="Headroom"
          name="capacity_headroom"
          type="number"
          step={0.05}
          min={0.05}
          max={10}
          defaultValue={String(model.capacity_headroom ?? 1)}
          hint="Above 1 lets requests queue at the backend, for autoscalers that scale on queue depth."
        />
      </div>
      <CapacityDiscoveryStatus status={status} entry={entry} mode={view?.mode} replicas={view?.replicas} enabled={view?.enabled} />
    </div>
  );
}

/**
 * The model's live occupancy against its pool: cluster-wide with shared
 * slots, otherwise the answering gateway's own count against what it
 * enforces, with a note when that is a split share or a fallback.
 */
export function capacityInFlight(
  entry: Pick<CapacityDiscoveryModelView, "enforced_max_in_flight" | "cluster_in_flight" | "in_flight"> | undefined,
  effective: number,
  mode: FairshareSlotMode | undefined,
  replicas: number | undefined,
): { value: string; note: string | null; fallback: boolean } {
  if (!entry) return { value: "—", note: null, fallback: false };
  const n = Math.max(replicas ?? 1, 1);
  if (mode === "shared") {
    const cluster = entry.cluster_in_flight == null ? "—" : String(entry.cluster_in_flight);
    return { value: `${cluster} of ${effective} (cluster-wide, ${n} gateways)`, note: `this gateway ${entry.in_flight}`, fallback: false };
  }
  const value = `${entry.in_flight} of ${entry.enforced_max_in_flight}`;
  if (mode === "fallback") {
    return { value, note: `fallback: shared slots unavailable, this gateway's own limit (${n} gateways)`, fallback: true };
  }
  if (mode === "split" || (mode === undefined && n > 1 && entry.enforced_max_in_flight < effective)) {
    return { value, note: `this gateway's share, split across ${n} gateways`, fallback: false };
  }
  return { value, note: null, fallback: false };
}

function formatTime(value: string) {
  return new Date(value).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

export function CapacityDiscoveryStatus({
  status,
  entry,
  mode,
  replicas,
  enabled,
}: {
  status?: CapacityDiscoveryView["models"][number]["status"];
  entry?: Pick<CapacityDiscoveryModelView, "enforced_max_in_flight" | "cluster_in_flight" | "in_flight">;
  mode?: FairshareSlotMode;
  replicas?: number;
  enabled?: boolean;
}) {
  if (!status) {
    return <p className="text-xs text-muted-foreground">Waiting for the gateway&apos;s discovery state…</p>;
  }
  const inFlight = capacityInFlight(entry, status.effective_max_in_flight, mode, replicas);
  const perReplica =
    status.per_replica_max_in_flight == null
      ? status.replica_capacity != null
        ? `varies, ${status.replica_capacity} summed${status.per_replica_source ? ` (${status.per_replica_source})` : ""}`
        : "—"
      : `${status.per_replica_max_in_flight}${status.per_replica_source ? ` (${status.per_replica_source})` : ""}`;
  return (
    <div className="rounded-lg border border-border p-3 text-xs" aria-label="Discovery status">
      <div className="flex flex-wrap items-center gap-2">
        <span
          className={cn(
            "inline-flex h-[22px] items-center rounded-full border px-2 text-[11.5px] font-medium",
            status.state === "discovered" ? "border-border text-secondary-foreground" : "border-foreground bg-foreground text-background",
          )}
        >
          {status.state}
        </span>
        <span className="text-muted-foreground">
          source {status.source}
          {status.service ? ` · Service ${status.service}` : ""}
          {status.namespace ? ` in ${status.namespace}` : status.namespaces.length ? ` in ${status.namespaces.join(", ")}` : ""}
        </span>
        {enabled === false && <span className="text-muted-foreground">· discovery is off on this gateway</span>}
      </div>
      <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 md:grid-cols-3">
        <div><dt className="text-muted-foreground">Ready replicas</dt><dd className="tabular-nums">{status.ready_replicas ?? "—"}</dd></div>
        <div><dt className="text-muted-foreground">Per replica</dt><dd className="tabular-nums">{perReplica}</dd></div>
        <div><dt className="text-muted-foreground">Headroom</dt><dd className="tabular-nums">×{status.headroom}</dd></div>
        <div><dt className="text-muted-foreground">Derived (cluster-wide)</dt><dd className="tabular-nums">{status.derived_max_in_flight ?? "—"}</dd></div>
        <div><dt className="text-muted-foreground">In force (cluster-wide)</dt><dd className="tabular-nums">{status.effective_max_in_flight}</dd></div>
        <div>
          <dt className="text-muted-foreground">In flight</dt>
          <dd className="tabular-nums" data-testid="capacity-in-flight">
            {inFlight.value}
            {inFlight.note && <span className={cn("block", inFlight.fallback ? "text-foreground" : "text-muted-foreground")}>{inFlight.note}</span>}
          </dd>
        </div>
        <div><dt className="text-muted-foreground">Last refresh</dt><dd>{status.last_refresh ? formatTime(status.last_refresh) : "—"}</dd></div>
        <div><dt className="text-muted-foreground">Last discovered</dt><dd>{status.last_success ? formatTime(status.last_success) : "—"}</dd></div>
      </dl>
      {status.reason && <p className="mt-2 text-foreground">{status.reason}</p>}
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

function AutotuneField({ label, info, children }: { label: string; info: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center gap-3 p-3">
      <div className="flex w-44 shrink-0 items-center gap-1.5 text-xs font-medium">
        {label}
        <HelpTip label={`About ${label.toLowerCase()}`}>{info}</HelpTip>
      </div>
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

/**
 * Ramps live load against the upstream to find the in-flight knee, then
 * offers to apply it. An action rather than a setting: applying writes the
 * cap and switches the model to the tuned mode straight away.
 */
export function AutotuneButton({ model, disabled }: { model: ModelRoute; disabled?: boolean }) {
  const [open, setOpen] = useState(false);
  const [headroom, setHeadroom] = useState("4");
  const [replicas, setReplicas] = useState("1");
  const [workload, setWorkload] = useState<AutotuneWorkload>("chat");
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [report, setReport] = useState<AutotuneReport | null>(null);
  const [pending, start] = useTransition();

  const reset = () => {
    setReport(null);
    setError(null);
  };

  const runProbe = async () => {
    setRunning(true);
    setError(null);
    setReport(null);
    try {
      setReport(await autotuneModelAction(model.id, {
        latency_headroom: Math.max(1.5, Number(headroom) || 4),
        replicas: Math.max(1, Math.round(Number(replicas) || 1)),
        workload,
      }));
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
    <Dialog open={open} onOpenChange={(next) => { setOpen(next); if (!next) reset(); }}>
      <DialogTrigger asChild>
        <Button type="button" size="sm" variant="outline" disabled={disabled}>
          <Zap className="h-3.5 w-3.5" />
          Auto-tune
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-3xl">
        <DialogHeader>
          <DialogTitle>Auto-tune {model.model_name}</DialogTitle>
          <DialogDescription>
            Sends real requests straight to the upstream and ramps concurrency to find its true capacity.{" "}
            <span className="font-medium text-foreground">Consumes upstream tokens</span> — avoid running against a busy production model.
          </DialogDescription>
        </DialogHeader>
        <div className="divide-y divide-border rounded-md border border-border">
          <AutotuneField label="Replicas running" info="How many copies of the model are serving traffic. The probe scales its concurrency ceiling (up to ~32× this) so the ramp covers a realistic load for your fleet.">
            <TextField name="autotune_replicas" label="Replicas running" type="number" min={1} step={1} value={replicas} onChange={(e) => setReplicas(e.target.value)} />
          </AutotuneField>
          <AutotuneField label="Latency tolerance" info="How much slower than a single idle request you'll accept at peak load. The ramp stops once p99 crosses this multiple of the baseline — tighter tolerance means fewer slots but snappier responses.">
            <Select aria-label="Latency tolerance" value={headroom} onValueChange={setHeadroom} className="h-9 w-full text-xs" options={AUTOTUNE_HEADROOM_OPTIONS} />
          </AutotuneField>
          <AutotuneField label="Workload" info="The shape of the probe requests. Coding sends large prompts and longer replies, which costs more capacity than short chat turns, so it tunes to a lower slot count.">
            <Select
              aria-label="Workload"
              value={workload}
              onValueChange={(value) => setWorkload(value as AutotuneWorkload)}
              className="h-9 w-full text-xs"
              contentClassName="w-72"
              options={[
                { value: "chat", label: "Chat — short prompt, short reply" },
                { value: "coding", label: "Coding — large context, longer reply" },
              ]}
            />
          </AutotuneField>
        </div>
        {error && <p className="rounded-md border border-foreground/40 p-2 text-xs">{error}</p>}
        {report && (
          <div className="space-y-3">
            <div className="flex flex-wrap items-center gap-x-6 gap-y-1 rounded-md border border-border p-3 text-xs">
              <div><span className="text-muted-foreground">Recommended slots</span><p className="text-lg font-semibold tabular-nums">{report.recommended_max_in_flight}</p></div>
              <div><span className="text-muted-foreground">Throughput at knee</span><p className="text-lg font-semibold tabular-nums">{report.recommended_throughput_rps.toFixed(1)} rps</p></div>
              <div>
                <span className="text-muted-foreground">Latency budget</span>
                <p className="font-medium tabular-nums">
                  {report.baseline_p99_ms > 0 ? `${report.baseline_p99_ms} ms → ${report.latency_ceiling_ms} ms (${report.latency_headroom.toFixed(0)}×)` : "no baseline"}
                </p>
              </div>
              <div className="min-w-[12rem] flex-1"><span className="text-muted-foreground">Why it stopped</span><p className="font-medium">{AUTOTUNE_KNEE_LABEL[report.knee_reason]}</p></div>
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
                      <tr key={step.concurrency} className={cn("border-b border-border/50", isRec && "bg-secondary")}>
                        <td className="py-1.5 pl-3 pr-3 tabular-nums">{step.concurrency}{isRec && <span className="ml-1">★</span>}</td>
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
              {running ? <><RefreshCw className="h-3.5 w-3.5 animate-spin" />Probing…</> : <><Zap className="h-3.5 w-3.5" />Run probe</>}
            </Button>
          ) : (
            <div className="flex w-full items-center justify-end gap-2">
              <Button type="button" size="sm" variant="secondary" onClick={runProbe} disabled={running || pending}>Re-run</Button>
              <Button type="button" size="sm" onClick={apply} disabled={pending || report.knee_reason === "no_data"}>
                {pending ? "Applying…" : `Apply ${report.recommended_max_in_flight} slots`}
              </Button>
            </div>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
