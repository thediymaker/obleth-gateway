"use client";

import { useCallback, useEffect, useMemo, useRef, useState, useTransition, type ReactNode } from "react";
import { useRouter } from "next/navigation";
import { saveModelSettingsAction } from "@/app/actions";
import { AutotuneButton, CapacityDiscoveryFields, capacityMode, CapacityModeToggle, type CapacityMode } from "@/components/models/capacity";
import {
  ALIASES_HINT,
  ChangesContext,
  ChatCapabilityFields,
  MODEL_TYPE_OPTIONS,
  perMillion,
  fromPerMillion,
  QUANTIZATION_HINT,
  QUANTIZATION_OPTIONS,
  RoutingTagsField,
  SelectField,
  Setting,
  Switch,
  TextArea,
  TextField,
  toPlainDecimal,
  UPSTREAM_HEADERS_HINT,
  VARIANTS_HINT,
  VariantsField,
} from "@/components/models/fields";
import { Button } from "@/components/ui/button";
import type { BoonBlockers } from "@/lib/boon-availability";
import { changedFields, changedSections, changeSummary, fieldSection, snapshotForm, type FormSnapshot, type SettingsSectionId } from "@/lib/models-model";
import type { McpServer, ModelHealthCheck, ModelHealthSummary, ModelRoute } from "@/lib/obleth";
import { upstreamHeadersText } from "@/lib/upstream-headers";
import { cn } from "@/lib/utils";

/** A settings card on the model page; `data-section` lets the page find which cards hold changes. */
function Section({ id, title, description, children }: { id: SettingsSectionId; title: string; description?: ReactNode; children: ReactNode }) {
  return (
    <section id={id} data-section={id} aria-label={title} className="scroll-mt-24 rounded-xl border border-border bg-card">
      <header className="px-[18px] pb-3 pt-4">
        <h2 className="text-sm font-semibold">{title}</h2>
        {description && <p className="mt-0.5 text-xs text-muted-foreground">{description}</p>}
      </header>
      <div className="border-t border-border">{children}</div>
    </section>
  );
}

/**
 * A price edited per million (or per item), submitted as the per-unit value
 * the gateway stores. Untouched, the hidden field carries the stored value
 * exactly, so opening the page never reads as a change.
 */
function PriceInput({ name, label, value, perM }: { name: string; label: string; value: number; perM: boolean }) {
  const start = perM ? perMillion(value) : value > 0 ? toPlainDecimal(value) : "";
  const [shown, setShown] = useState(start);
  const stored = shown === start ? toPlainDecimal(value) : perM ? fromPerMillion(shown) || "0" : shown.trim() || "0";
  return (
    <label className="flex min-w-0 flex-col gap-1">
      <span className="text-[11.5px] text-muted-foreground">{label}</span>
      <span className="flex h-9 items-center rounded-md border border-input bg-background pl-3 text-[13px] focus-within:ring-1 focus-within:ring-ring">
        <span className="text-muted-foreground">$</span>
        <input aria-label={label} inputMode="decimal" value={shown} onChange={(e) => setShown(e.target.value)} placeholder="0" className="h-full min-w-0 flex-1 bg-transparent px-1.5 font-mono text-[12.5px] outline-none" />
      </span>
      <input type="hidden" name={name} value={stored} />
    </label>
  );
}

function prices(type: string, m: ModelRoute): { name: string; label: string; value: number; perM: boolean }[] {
  switch (type) {
    case "chat":
      return [
        { name: "input_cost_per_token", label: "Input, per 1M tokens", value: m.input_cost_per_token, perM: true },
        { name: "output_cost_per_token", label: "Output, per 1M tokens", value: m.output_cost_per_token, perM: true },
      ];
    case "embedding":
      return [{ name: "input_cost_per_token", label: "Input, per 1M tokens", value: m.input_cost_per_token, perM: true }];
    case "image":
      return [{ name: "cost_per_image", label: "Per image", value: m.cost_per_image, perM: false }];
    case "video":
      return [{ name: "cost_per_video", label: "Per video", value: m.cost_per_video, perM: false }];
    case "audio_speech":
      return [{ name: "cost_per_character", label: "Per 1M characters", value: m.cost_per_character, perM: true }];
    case "audio_transcription":
      return [{ name: "cost_per_audio_second", label: "Per audio second", value: m.cost_per_audio_second, perM: false }];
    default:
      return [];
  }
}

function datetimeLocalValue(value: string | null) {
  if (!value) return "";
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** Fields that arrive after the first paint (the attachment list loads on its own) join the baseline instead of reading as edits. */
function adoptLateFields(base: FormSnapshot, next: FormSnapshot): FormSnapshot {
  if (base.has("knowledge_loaded") || !next.has("knowledge_loaded")) return base;
  const out = new Map(base);
  out.set("knowledge_loaded", next.get("knowledge_loaded")!);
  if (next.has("knowledge_collection")) out.set("knowledge_collection", next.get("knowledge_collection")!);
  return out;
}

export interface SettingsState {
  dirty: SettingsSectionId[];
  count: number;
}

export function ModelSettings({
  model,
  summary,
  checks,
  mcpServers,
  modelNames,
  boonBlockers,
  load,
  onStateChange,
}: {
  model: ModelRoute;
  summary: ModelHealthSummary;
  checks: ModelHealthCheck[];
  mcpServers: McpServer[];
  modelNames: string[];
  boonBlockers: BoonBlockers;
  load: { inFlight: number; cap: number; queued: number };
  onStateChange: (state: SettingsState) => void;
}) {
  const router = useRouter();
  const form = useRef<HTMLFormElement>(null);
  const [version, setVersion] = useState(0);
  const [baseline, setBaseline] = useState<FormSnapshot | null>(null);
  const [changed, setChanged] = useState<string[]>([]);
  const [pending, start] = useTransition();
  const [result, setResult] = useState<{ ok: boolean; text: string; warnings?: string[] } | null>(null);
  const [editType, setEditType] = useState(model.model_type || "chat");
  const [quantization, setQuantization] = useState(model.quantization || "unknown");
  const [selection, setSelection] = useState(model.endpoint_selection_mode);
  const [bias, setBias] = useState(String(model.route_bias));
  const [mode, setMode] = useState<CapacityMode>(capacityMode(model.capacity_mode));
  const frame = useRef(0);
  const baseRef = useRef<FormSnapshot | null>(null);
  baseRef.current = baseline;

  const read = useCallback(() => (form.current ? snapshotForm(new FormData(form.current)) : null), []);

  const recompute = useCallback(() => {
    cancelAnimationFrame(frame.current);
    frame.current = requestAnimationFrame(() => {
      const next = read();
      const base = baseRef.current;
      if (!next || !base) return;
      const adopted = adoptLateFields(base, next);
      if (adopted !== base) setBaseline(adopted);
      const fields = changedFields(adopted, next).filter((f) => next.has("knowledge_loaded") || fieldSection(f) !== "knowledge");
      setChanged((prev) => (prev.join("|") === fields.join("|") ? prev : fields));
    });
  }, [read]);

  // The baseline is the form as first painted (and again after a discard,
  // which remounts it from the stored model).
  useEffect(() => {
    const snap = read();
    setBaseline(snap);
    setChanged([]);
  }, [read, version]);

  // Controlled pickers and lists that appear on a toggle change the form
  // without an input event, so watch the DOM as well.
  useEffect(() => {
    const el = form.current;
    if (!el) return;
    const observer = new MutationObserver(recompute);
    observer.observe(el, { subtree: true, childList: true, attributes: true, attributeFilter: ["value", "checked"] });
    return () => observer.disconnect();
  }, [recompute, version]);

  // Which cards hold changes, for the section list's dots.
  useEffect(() => {
    const el = form.current;
    const dirty = el ? [...el.querySelectorAll<HTMLElement>("[data-section]")].filter((s) => s.querySelector("[data-changed]")).map((s) => s.dataset.section as SettingsSectionId) : [];
    onStateChange({ dirty, count: changed.length });
  }, [changed, onStateChange]);

  // Leaving with unsaved changes asks first: a reload or closed tab through
  // the browser, and in-app links through a click check.
  useEffect(() => {
    if (changed.length === 0) return;
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    const onClick = (e: MouseEvent) => {
      const a = e.target instanceof Element ? e.target.closest("a") : null;
      if (!a || a.target === "_blank" || e.metaKey || e.ctrlKey || e.shiftKey) return;
      const href = a.getAttribute("href") ?? "";
      if (href.startsWith("#") || !href) return;
      if (!window.confirm("You have unsaved changes on this model. Leave without saving them?")) {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    document.addEventListener("click", onClick, true);
    return () => {
      window.removeEventListener("beforeunload", onBeforeUnload);
      document.removeEventListener("click", onClick, true);
    };
  }, [changed.length]);

  const sections = changedSections(changed);
  const summaryLabels = changeSummary(changed);

  function save() {
    const el = form.current;
    if (!el || sections.length === 0) return;
    if (!el.reportValidity()) return;
    const data = new FormData(el);
    data.set("id", model.id);
    data.set("sections", sections.join(","));
    const submitted = snapshotForm(data);
    setResult(null);
    start(async () => {
      const res = await saveModelSettingsAction(data);
      // Whatever saved is the new baseline; anything that failed stays marked.
      const savedSections = res.ok ? sections : res.saved;
      setBaseline((base) => {
        if (!base) return base;
        const next = new Map(base);
        for (const name of new Set([...base.keys(), ...submitted.keys()])) {
          const section = fieldSection(name);
          if (!section || !savedSections.includes(section)) continue;
          if (submitted.has(name)) next.set(name, submitted.get(name)!);
          else next.delete(name);
        }
        // The key is write-only: once saved, the field goes back to blank.
        if (savedSections.includes("model")) next.set("api_key", "");
        return next;
      });
      if (res.ok) {
        const key = el.querySelector<HTMLInputElement>('input[name="api_key"]');
        if (key) key.value = "";
        setResult({ ok: true, text: "Saved.", warnings: res.warnings });
      } else {
        setResult({ ok: false, text: res.saved.length ? `${res.error} The other changes were saved.` : res.error });
      }
      recompute();
      router.refresh();
    });
  }

  function discard() {
    setResult(null);
    setEditType(model.model_type || "chat");
    setQuantization(model.quantization || "unknown");
    setSelection(model.endpoint_selection_mode);
    setBias(String(model.route_bias));
    setMode(capacityMode(model.capacity_mode));
    setVersion((v) => v + 1);
  }

  // A plain "Saved." clears itself; warnings and errors stay until dismissed.
  useEffect(() => {
    if (!result?.ok || result.warnings?.length) return;
    const t = setTimeout(() => setResult(null), 2500);
    return () => clearTimeout(t);
  }, [result]);

  const chat = editType === "chat";
  const textual = chat || editType === "embedding";
  const context = useMemo(() => ({ changed, initial: baseline }), [changed, baseline]);
  const priceFields = prices(editType, model);

  return (
    <ChangesContext.Provider value={context}>
      <form
        key={version}
        ref={form}
        onSubmit={(e) => { e.preventDefault(); save(); }}
        onInput={recompute}
        onChange={recompute}
        onClick={recompute}
        onKeyUp={recompute}
        className="flex flex-col gap-4"
        aria-label={`${model.model_name} settings`}
      >
        <Section id="general" title="General" description="What the model is and what else it answers to.">
          <Setting id="set-description" label="Description" hint="Shown in the model list and the Playground." fields={["description"]} was={{ field: "description" }}>
            <TextField name="description" label="Description" defaultValue={model.description} placeholder="What it's good for" />
          </Setting>
          <Setting id="set-type" label="Model type" hint="Which API it serves and how it bills." fields={["model_type"]} was={{ field: "model_type" }}>
            <div className="max-w-xs"><SelectField name="model_type" label="Model type" value={editType} onChange={setEditType} options={MODEL_TYPE_OPTIONS} /></div>
            <p className="text-[11.5px] text-muted-foreground">{editType === model.model_type ? "" : "Changing the type changes which endpoints the model serves and which price applies."}</p>
          </Setting>
          <Setting id="set-aliases" label="Aliases" hint="Other names that reach this model, one per line." help={ALIASES_HINT} fields={["aliases"]}>
            <TextArea name="aliases" label="Aliases" rows={2} defaultValue={(model.aliases ?? []).join("\n")} placeholder={"old-name\nanother-name"} />
          </Setting>
          {(chat || (model.variants?.length ?? 0) > 0) && (
            <Setting id="set-variants" label="Variants" hint="Other names for this model with extra boons turned on." help={VARIANTS_HINT} fields={["variants"]}>
              <VariantsField model={model} modelNames={modelNames} />
            </Setting>
          )}
          <Setting id="set-quantization" label="Quantization" hint="The weights' format, for reference only." help={QUANTIZATION_HINT} fields={["quantization"]} was={{ field: "quantization" }}>
            <div className="max-w-xs"><SelectField name="quantization" label="Quantization" value={quantization} onChange={setQuantization} options={QUANTIZATION_OPTIONS} /></div>
          </Setting>
        </Section>

        <Section id="connection" title="Connection" description="Where requests go, and how hard the gateway tries.">
          <Setting id="set-upstream" label="Upstream model" hint="The name the endpoint itself serves." fields={["upstream_model"]} was={{ field: "upstream_model" }}>
            <TextField name="upstream_model" label="Upstream model" defaultValue={model.upstream_model} required mono spellCheck={false} />
          </Setting>
          <Setting id="set-api-base" label="API base URL" hint={model.api_base ? "OpenAI-compatible base, usually ending in /v1." : "Blank: the provisioner adds its replicas as endpoints."} fields={["api_base"]} was={{ field: "api_base" }}>
            <TextField name="api_base" label="API base URL" defaultValue={model.api_base} mono spellCheck={false} placeholder="http://my-inference-server:8000/v1" />
          </Setting>
          <Setting id="set-api-key" label="Upstream API key" hint={model.api_key_set ? "A key is stored. Type a new one to replace it." : "No key stored."} fields={["api_key"]}>
            <TextField name="api_key" label="Upstream API key" type="password" autoComplete="new-password" placeholder={model.api_key_set ? "•••••••• (stored)" : "sk-…"} />
          </Setting>
          <Setting id="set-headers" label="Upstream headers" hint="Sent on every request, one Name: value per line." help={UPSTREAM_HEADERS_HINT} fields={["upstream_headers"]}>
            <TextArea name="upstream_headers" label="Upstream headers" rows={2} defaultValue={upstreamHeadersText(model.upstream_header_names)} placeholder="x-routing-hint: sticky" />
          </Setting>
          <Setting id="set-timeout" label="Request timeout" hint="Seconds before the gateway gives up on a request. Blank uses the gateway default." fields={["request_timeout_secs"]} was={{ field: "request_timeout_secs" }}>
            <TextField name="request_timeout_secs" label="Request timeout in seconds" type="number" min={1} className="max-w-[10rem]" defaultValue={model.request_timeout_secs == null ? "" : String(model.request_timeout_secs)} placeholder="default" />
          </Setting>
          <Setting id="set-retries" label="Max retries" hint="How many more times to try, on this or another endpoint, and how long to wait between tries." fields={["max_retries", "retry_backoff_ms"]}>
            <div className="flex flex-wrap items-center gap-2 text-[12.5px] text-muted-foreground">
              <TextField name="max_retries" label="Max retries" type="number" min={0} className="w-24" defaultValue={String(model.max_retries)} />
              retries,
              <TextField name="retry_backoff_ms" label="Retry backoff in milliseconds" type="number" min={0} className="w-28" defaultValue={String(model.retry_backoff_ms)} />
              ms apart
            </div>
          </Setting>
          <Setting id="set-selection" label="Endpoint selection" hint="How a request picks between this model's endpoints." fields={["endpoint_selection_mode"]} was={{ field: "endpoint_selection_mode" }}>
            <div className="max-w-sm">
              <SelectField
                name="endpoint_selection_mode"
                label="Endpoint selection"
                value={selection}
                onChange={setSelection}
                options={[
                  { value: "failover", label: "Failover", hint: "In priority order" },
                  { value: "load_balance", label: "Load balance", hint: "Weighted" },
                  { value: "session_hash", label: "Sticky by session", hint: "The same session lands on the same endpoint" },
                ]}
              />
            </div>
          </Setting>
          <Setting id="set-debug" label="Debug upstream failures" hint="Keep the upstream's answer when a request ends in a 502 or 504." fields={["debug_diagnostics"]} was={{ field: "debug_diagnostics", checkbox: true }}>
            <Switch name="debug_diagnostics" label="Debug upstream failures" defaultChecked={model.debug_diagnostics} />
          </Setting>
        </Section>

        {chat && (
          <Section id="routing" title="Routing" description="How the auto router treats this model. It used to live under Connection → Cost.">
            <Setting id="set-auto" label="Eligible for auto" hint={<>The router may send <code className="font-mono">model: auto</code> requests here. Off keeps it reachable by name.</>} fields={["auto_eligible"]} was={{ field: "auto_eligible", checkbox: true }}>
              <input type="hidden" name="has_routing" value="1" />
              <Switch name="auto_eligible" label="Eligible for auto" defaultChecked={model.auto_eligible} />
            </Setting>
            <Setting
              id="set-tags"
              label="Router tags"
              hint="What it's good at. A higher level is preferred when a request needs that strength."
              help="The router matches request intent against these tags. Auto derives the level from the model's cost rank; Basic, Strong and Best pin it. The router prefers the cheapest model strong enough for a request. The vision tag marks native image support."
              fields={["tag_"]}
            >
              <RoutingTagsField model={model} />
            </Setting>
            <Setting id="set-bias" label="Routing bias" hint="Multiplies the model's routing score. ×1.0 is neutral." fields={["route_bias"]} was={{ field: "route_bias" }}>
              <div className="flex items-center gap-3">
                <input name="route_bias" type="range" min={0.1} max={3} step={0.1} value={bias} onChange={(e) => setBias(e.target.value)} aria-label="Routing bias" className="w-72 accent-foreground" />
                <span className="font-mono text-[13px] tabular-nums">×{Number(bias).toFixed(1)}</span>
              </div>
            </Setting>
          </Section>
        )}

        {textual && (
          <Section id="capabilities" title="Capabilities" description={chat ? "What the model can do itself, and what the gateway adds on top." : "How much the model reads at once."}>
            <Setting id="set-context" label="Context window" hint="Tokens per request, prompt and reply together." fields={["context_window"]} was={{ field: "context_window" }}>
              <TextField name="context_window" label="Context window" type="number" min={1} className="max-w-[12rem]" defaultValue={String(model.context_window)} />
            </Setting>
            {chat && <ChatCapabilityFields model={model} mcpServers={mcpServers} modelNames={modelNames} selfName={model.model_name} boonBlockers={boonBlockers} layout="rows" />}
          </Section>
        )}

        <Section id="pricing" title="Pricing" description="What each request costs its tenant.">
          <Setting id="set-price" label="Price" hint="Billed from the price at the time of each request." fields={["input_cost_per_token", "output_cost_per_token", "cost_per_"]}>
            {priceFields.length ? (
              <div className="grid max-w-md gap-3 sm:grid-cols-2">{priceFields.map((p) => <PriceInput key={`${editType}-${p.name}`} {...p} />)}</div>
            ) : (
              <p className="text-xs text-muted-foreground">No price for this type.</p>
            )}
          </Setting>
          <Setting
            id="set-energy"
            label="Energy slots per node"
            hint="0 turns energy accounting off for this model."
            help="How many of this model's requests one node serves at once when fully loaded (replicas per node × concurrent sequences per replica). Node power is split across this many slots."
            fields={["energy_slots_per_node"]}
            was={{ field: "energy_slots_per_node" }}
          >
            <TextField name="energy_slots_per_node" label="Energy slots per node" type="number" min={0} className="max-w-[10rem]" defaultValue={String(model.energy_slots_per_node)} />
          </Setting>
        </Section>

        <Section id="capacity" title="Capacity" description={model.enabled ? (load.cap > 0 ? `${load.inFlight} of ${load.cap} slots in use${load.queued ? `, ${load.queued} waiting` : ""} right now.` : `${load.inFlight} in flight, no cap.`) : "Off: takes no requests."}>
          <Setting
            id="set-capacity-mode"
            label="Capacity mode"
            hint={mode === "discovered" ? "The pool follows the replicas the backend reports." : mode === "tuned" ? "The pool uses the last auto-tune result." : "The pool is the number you set."}
            help="Static uses Max slots. Tuned uses the auto-tune result. Discovered counts the backend's ready replicas and multiplies by the requests one replica takes."
            fields={["capacity_mode"]}
            was={{ field: "capacity_mode" }}
          >
            <CapacityModeToggle mode={mode} onChange={setMode} />
          </Setting>
          {mode === "discovered" && (
            <Setting id="set-discovery" label="Discovered capacity" hint="Where replicas are counted, and what one takes." fields={["capacity_source", "capacity_service", "capacity_namespace", "per_replica_max_in_flight", "capacity_headroom"]}>
              <CapacityDiscoveryFields model={model} />
            </Setting>
          )}
          <Setting
            id="set-max-slots"
            label={mode === "discovered" ? "Fallback slots" : "Max slots"}
            hint={mode === "discovered" ? "Used while discovery has no answer." : "Requests sent to the upstream at once. Blank uses the gateway default (OBLETH_DEFAULT_MODEL_MAX_IN_FLIGHT, 32 unless set)."}
            fields={["max_in_flight"]}
            was={{ field: "max_in_flight" }}
          >
            <TextField name="max_in_flight" label={mode === "discovered" ? "Fallback slots" : "Max slots"} type="number" min={1} className="max-w-[10rem]" defaultValue={model.max_in_flight == null ? "" : String(model.max_in_flight)} placeholder="No cap" />
          </Setting>
          <Setting id="set-weight" label="Admission weight" hint="This model's share of the gateway when demand is higher than supply." fields={["admission_weight"]} was={{ field: "admission_weight" }}>
            <TextField name="admission_weight" label="Admission weight" type="number" min={1} className="max-w-[10rem]" defaultValue={String(model.admission_weight)} />
          </Setting>
          <Setting id="set-cache" label="Response cache" hint="Answer repeated identical requests from the cache." fields={["cache_enabled", "cache_ttl_secs"]}>
            <div className="flex flex-wrap items-center gap-3 text-[12.5px] text-muted-foreground">
              <Switch name="cache_enabled" label="Response cache" defaultChecked={model.cache_enabled} />
              keep answers for
              <TextField name="cache_ttl_secs" label="Cache lifetime in seconds" type="number" min={1} className="w-24" defaultValue={String(model.cache_ttl_secs || 300)} />
              seconds
            </div>
          </Setting>
          {textual && (
            <Setting id="set-autotune" label="Auto-tune capacity" hint="Ramp real load against the upstream to find how many requests it takes. Applying switches to the tuned mode straight away.">
              <div><AutotuneButton model={model} disabled={pending} /></div>
            </Setting>
          )}
        </Section>

        <Section id="health" title="Health" description="How often the gateway checks the model, and who hears about it.">
          <Setting id="set-checks" label="Health checks" hint="Scheduled checks, and a Slack alert when they fail." fields={["checks_enabled", "alerts_enabled", "check_interval_secs"]}>
            <div className="grid max-w-xl gap-2 sm:grid-cols-2">
              <Switch name="checks_enabled" label="Scheduled checks" defaultChecked={summary.checks_enabled}>Scheduled checks</Switch>
              <Switch name="alerts_enabled" label="Slack alerts" defaultChecked={summary.alerts_enabled}>Slack alerts</Switch>
            </div>
            <div className="flex items-center gap-2 text-[12.5px] text-muted-foreground">
              every
              <TextField name="check_interval_secs" label="Check interval in seconds" type="number" min={1} className="w-28" defaultValue={String(summary.check_interval_secs)} />
              seconds
            </div>
          </Setting>
          <Setting id="set-threshold" label="Failure threshold" hint="Failed checks in a row before the model counts as down and alerts." fields={["failure_threshold"]} was={{ field: "failure_threshold" }}>
            <TextField name="failure_threshold" label="Failure threshold" type="number" min={1} className="max-w-[8rem]" defaultValue={String(summary.failure_threshold)} />
          </Setting>
          <Setting id="set-maintenance" label="Maintenance window" hint="Until then failed checks don't alert, and the model shows as in maintenance." fields={["maintenance_until", "maintenance_note"]}>
            <div className="grid max-w-xl gap-2 sm:grid-cols-[14rem_minmax(0,1fr)]">
              <TextField name="maintenance_until" label="Maintenance until" type="datetime-local" defaultValue={datetimeLocalValue(summary.maintenance_until)} />
              <TextField name="maintenance_note" label="Maintenance note" defaultValue={summary.maintenance_note ?? ""} placeholder="Note (optional)" />
            </div>
          </Setting>
          <Setting label="Recent checks" hint="The latest results, newest first.">
            <RecentChecks checks={checks} />
          </Setting>
        </Section>
      </form>

      {(changed.length > 0 || result) && (
        <div role="region" aria-label="Unsaved changes" className="sticky bottom-4 z-30 flex flex-wrap items-center justify-between gap-3 rounded-xl border border-muted-foreground/50 bg-card px-4 py-3 shadow-2xl">
          <div className="flex min-w-0 flex-wrap items-center gap-3 text-[13px]">
            {changed.length > 0 && (
              <>
                <span className="inline-flex h-[22px] items-center rounded-full bg-foreground px-2 text-[11.5px] font-semibold text-background">{summaryLabels.length} unsaved</span>
                <span className="min-w-0 truncate text-secondary-foreground">{summaryLabels.join(" · ")}</span>
              </>
            )}
            {result && (
              <span role="status" className={cn("min-w-0", result.ok ? "text-secondary-foreground" : "font-medium text-foreground")}>
                {result.text}
                {result.warnings?.length ? <span className="block text-xs text-muted-foreground">Saved, but the upstream disagrees: {result.warnings.join(" ")}</span> : null}
              </span>
            )}
          </div>
          <div className="flex items-center gap-2">
            {result && !result.ok && changed.length === 0 && <Button type="button" size="sm" variant="ghost" onClick={() => setResult(null)}>Dismiss</Button>}
            {result?.warnings?.length && changed.length === 0 ? <Button type="button" size="sm" variant="ghost" onClick={() => setResult(null)}>Dismiss</Button> : null}
            {changed.length > 0 && (
              <>
                <Button type="button" size="sm" variant="outline" disabled={pending} onClick={discard}>Discard</Button>
                <Button type="button" size="sm" disabled={pending} onClick={save}>{pending ? "Saving…" : "Save changes"}</Button>
              </>
            )}
          </div>
        </div>
      )}
    </ChangesContext.Provider>
  );
}

function RecentChecks({ checks }: { checks: ModelHealthCheck[] }) {
  if (checks.length === 0) return <p className="text-xs text-muted-foreground">No checks yet.</p>;
  return (
    <div className="max-h-64 overflow-auto rounded-lg border border-border">
      <table className="w-full text-xs">
        <thead className="sticky top-0 bg-card">
          <tr className="border-b border-border text-left text-[10.5px] uppercase tracking-[0.07em] text-muted-foreground">
            <th className="py-2 pl-3 pr-3 font-semibold">Time</th>
            <th className="py-2 pr-3 font-semibold">Result</th>
            <th className="py-2 pr-3 font-semibold">HTTP</th>
            <th className="py-2 pr-3 font-semibold">Latency</th>
            <th className="py-2 pr-3 font-semibold">Note</th>
          </tr>
        </thead>
        <tbody>
          {checks.slice(0, 20).map((c) => (
            <tr key={c.id} className="border-b border-border/60 last:border-b-0">
              <td className="whitespace-nowrap py-1.5 pl-3 pr-3 tabular-nums text-muted-foreground">{new Date(c.checked_at).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })}</td>
              <td className={cn("py-1.5 pr-3", c.status === "unhealthy" && "font-semibold")}>{c.status}{c.trigger !== "scheduled" ? <span className="text-muted-foreground"> · {c.trigger}</span> : null}</td>
              <td className="py-1.5 pr-3 tabular-nums text-muted-foreground">{c.http_status ?? "—"}</td>
              <td className="py-1.5 pr-3 tabular-nums text-muted-foreground">{c.latency_ms == null ? "—" : `${c.latency_ms} ms`}</td>
              <td className="max-w-[20rem] truncate py-1.5 pr-3 text-muted-foreground" title={c.message ?? ""}>{c.message ?? ""}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
