"use client";

import { memo, useCallback, useContext, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { saveModelSettingsAction } from "@/app/actions";
import { usePoolLoad } from "@/components/fairshare/hooks";
import { AutotuneButton, CapacityDiscoveryFields, capacityMode, CapacityModeToggle, type CapacityMode } from "@/components/models/capacity";
import { ALIASES_HINT, ChangesContext, ChatCapabilityFields, MODEL_TYPE_OPTIONS, fromPerMillion, QUANTIZATION_HINT, QUANTIZATION_OPTIONS, RoutingTagsField, SelectField, Setting, toPerMillion, TextArea, TextField, toPlainDecimal, UPSTREAM_HEADERS_HINT, VARIANTS_HINT, VariantsField } from "@/components/models/fields";
import { SettingsCard } from "@/components/ui/settings-card";
import { type SaveResult, SettingsForm } from "@/components/ui/settings-form";
import type { BoonBlockers } from "@/lib/boon-availability";
import { checkStatusLabel, fieldLabel, fieldSection, type FormSnapshot, type SettingsSectionId } from "@/lib/models-model";
import type { McpServer, ModelHealthCheck, ModelHealthSummary, ModelRoute } from "@/lib/obleth";
import { upstreamHeadersText } from "@/lib/upstream-headers";
import { cn } from "@/lib/utils";
import { dateTime } from "@/lib/format";
import { Switch } from "@/components/ui/switch";

/**
 * A price edited per million (or per item), submitted as the per-unit value
 * the gateway stores. Untouched, the hidden field carries the stored value
 * exactly, so opening the page never reads as a change.
 */
function PriceInput({ name, label, value, perM }: { name: string; label: string; value: number; perM: boolean }) {
  const start = perM ? toPerMillion(value) : value > 0 ? toPlainDecimal(value) : "";
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
    case "mesh":
      // Meshes share the flat per-job price field with videos.
      return [{ name: "cost_per_video", label: "Per mesh", value: m.cost_per_video, perM: false }];
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

/**
 * The knowledge list loads on its own: until it has, its field can't read as
 * an edit, and when it arrives it joins the baseline.
 */
function adoptKnowledge(base: FormSnapshot, now: FormSnapshot): FormSnapshot {
  if (base.has("knowledge_loaded")) return base;
  const loaded = now.has("knowledge_loaded");
  if (!loaded && (now.get("knowledge_collection") ?? "") === (base.get("knowledge_collection") ?? "")) return base;
  const out = new Map(base);
  if (loaded) out.set("knowledge_loaded", now.get("knowledge_loaded")!);
  if (now.has("knowledge_collection")) out.set("knowledge_collection", now.get("knowledge_collection")!);
  else out.delete("knowledge_collection");
  return out;
}

/** The upstream key is write-only: once the model saves, its field goes back to blank. */
const clearKey = (saved: string[]) => (saved.includes("model") ? ["api_key"] : []);

export interface SettingsState {
  dirty: SettingsSectionId[];
  count: number;
}

/**
 * Every settings card on a model's page, as one form. Memoised, with the live
 * load read in a leaf (CapacityNow), so a poll never re-renders the form.
 */
export const ModelSettings = memo(function ModelSettings({ model, onStateChange, ...cards }: CardsProps & { onStateChange: (state: SettingsState) => void }) {
  const router = useRouter();
  const box = useRef<HTMLDivElement>(null);

  const save = useCallback(async (data: FormData): Promise<SaveResult> => {
    const res = await saveModelSettingsAction(data);
    return res.ok && res.warnings?.length ? { ok: true, warnings: [`Saved, but the upstream disagrees: ${res.warnings.join(" ")}`] } : res;
  }, []);
  const refresh = useCallback(() => router.refresh(), [router]);
  // Which cards hold changes, for the section list's dots.
  const onDirtyChange = useCallback((_sections: string[], count: number) => {
    const dirty = [...(box.current?.querySelectorAll<HTMLElement>("[data-section]") ?? [])].filter((s) => s.querySelector("[data-changed]")).map((s) => s.dataset.section as SettingsSectionId);
    onStateChange({ dirty, count });
  }, [onStateChange]);

  return (
    <div ref={box}>
      <SettingsForm
        id={model.id}
        sectionOf={fieldSection}
        labelOf={fieldLabel}
        save={save}
        onSaved={refresh}
        onDirtyChange={onDirtyChange}
        adopt={adoptKnowledge}
        clearOnSave={clearKey}
        className="flex flex-col gap-4"
        ariaLabel={`${model.model_name} settings`}
      >
        <Cards model={model} {...cards} />
      </SettingsForm>
    </div>
  );
});

interface CardsProps {
  model: ModelRoute;
  summary: ModelHealthSummary;
  checks: ModelHealthCheck[];
  mcpServers: McpServer[];
  modelNames: string[];
  boonBlockers: BoonBlockers;
}

/** The cards themselves, inside the form, so Discard's remount also resets the pickers' state. */
function Cards({ model, summary, checks, mcpServers, modelNames, boonBlockers }: CardsProps) {
  const { saving } = useContext(ChangesContext);
  const [editType, setEditType] = useState(model.model_type || "chat");
  const [quantization, setQuantization] = useState(model.quantization || "unknown");
  const [selection, setSelection] = useState(model.endpoint_selection_mode);
  const [affinity, setAffinity] = useState(sessionAffinityMode(model.session_affinity));
  const [bias, setBias] = useState(String(model.route_bias));
  const [mode, setMode] = useState<CapacityMode>(capacityMode(model.capacity_mode));
  const chat = editType === "chat";
  const textual = chat || editType === "embedding";
  const priceFields = prices(editType, model);

  return (
    <>
      <SettingsCard id="general" title="General" description="What the model is and what else it answers to.">
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
        <Setting id="set-listed" label="List in /v1/models" hint="Callers see it in /v1/models and /model/info. Turn it off to keep it out of those lists while it still answers to its name, for example when a model family stands for it." fields={["listed"]} was={{ field: "listed", checkbox: true }}>
          <Switch name="listed" label="List in /v1/models" defaultChecked={model.listed ?? true} />
        </Setting>
        {(chat || (model.variants?.length ?? 0) > 0) && (
          <Setting id="set-variants" label="Variants" hint="Other names for this model with extra boons turned on." help={VARIANTS_HINT} fields={["variants"]}>
            <VariantsField model={model} modelNames={modelNames} />
          </Setting>
        )}
        <Setting id="set-quantization" label="Quantization" hint="The weights' format, for reference only." help={QUANTIZATION_HINT} fields={["quantization"]} was={{ field: "quantization" }}>
          <div className="max-w-xs"><SelectField name="quantization" label="Quantization" value={quantization} onChange={setQuantization} options={QUANTIZATION_OPTIONS} /></div>
        </Setting>
      </SettingsCard>

      <SettingsCard id="connection" title="Connection" description="Where requests go, and how hard the gateway tries.">
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
        <Setting id="set-rejected-fields" label="Rejected request fields" hint="Requests that include one of these fields get an error and are never sent to the model. Use it for an option the model's server can't handle. One field name per line." fields={["rejected_request_fields"]}>
          <TextArea name="rejected_request_fields" label="Rejected request fields" rows={2} defaultValue={(model.rejected_request_fields ?? []).join("\n")} placeholder="bad_words" />
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
      </SettingsCard>

      {chat && (
        <SettingsCard id="routing" title="Routing" description="How the auto router treats this model. It used to live under Connection → Cost.">
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
        </SettingsCard>
      )}

      {textual && (
        <SettingsCard id="capabilities" title="Capabilities" description={chat ? "What the model can do itself, and what the gateway adds on top." : "How much the model reads at once."}>
          <Setting id="set-context" label="Context window" hint="Tokens per request, prompt and reply together." fields={["context_window"]} was={{ field: "context_window" }}>
            <TextField name="context_window" label="Context window" type="number" min={1} className="max-w-[12rem]" defaultValue={String(model.context_window)} />
          </Setting>
          {chat && <ChatCapabilityFields model={model} mcpServers={mcpServers} modelNames={modelNames} selfName={model.model_name} boonBlockers={boonBlockers} />}
        </SettingsCard>
      )}

      <SettingsCard id="pricing" title="Pricing" description="What each request costs its tenant.">
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
          hint="Only used when Settings → Energy has no power of this model's own. 0 leaves it out."
          help="How many of this model's requests one node serves at once when fully loaded (replicas per node × concurrent sequences per replica). Node power is split across this many slots. A model with its own measured power ignores this."
          fields={["energy_slots_per_node"]}
          was={{ field: "energy_slots_per_node" }}
        >
          <TextField name="energy_slots_per_node" label="Energy slots per node" type="number" min={0} className="max-w-[10rem]" defaultValue={String(model.energy_slots_per_node)} />
        </Setting>
      </SettingsCard>

      <SettingsCard id="capacity" title="Capacity" description={<CapacityNow model={model} />}>
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
        <Setting
          id="set-affinity"
          label="Session affinity"
          hint="Lets an agent's next step go first and briefly keeps the slot it just freed, so the model's prompt cache still has its conversation. Other requests can wait a little longer."
          fields={["session_affinity_mode", "session_affinity_hold_ms", "session_affinity_max_held_percent", "session_affinity_max_bypass_secs"]}
        >
          <div className="max-w-sm">
            <SelectField
              name="session_affinity_mode"
              label="Session affinity"
              value={affinity}
              onChange={setAffinity}
              options={[
                { value: "gateway", label: "Gateway default", hint: "Follows OBLETH_FAIRSHARE_SESSION_AFFINITY" },
                { value: "on", label: "On", hint: "For this model, whatever the gateway default" },
                { value: "off", label: "Off", hint: "For this model, whatever the gateway default" },
              ]}
            />
          </div>
          {affinity === "on" && (
            <div className="mt-2 flex flex-wrap items-center gap-3 text-[12.5px] text-muted-foreground">
              keep a freed slot for
              <TextField name="session_affinity_hold_ms" label="Hold time in milliseconds" type="number" min={0} max={60000} className="w-24" defaultValue={optionalNumber(model.session_affinity?.hold_ms)} placeholder="default" />
              ms, for at most
              <TextField name="session_affinity_max_held_percent" label="Most of the slots kept at once, in percent" type="number" min={0} max={100} className="w-20" defaultValue={optionalNumber(model.session_affinity?.max_held_percent)} placeholder="default" />
              % of the slots; after
              <TextField name="session_affinity_max_bypass_secs" label="Wait limit in seconds" type="number" min={1} max={3600} className="w-20" defaultValue={optionalNumber(model.session_affinity?.max_bypass_secs)} placeholder="default" />
              seconds of waiting, nothing goes ahead of a request
            </div>
          )}
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
            <div><AutotuneButton model={model} disabled={saving} /></div>
          </Setting>
        )}
      </SettingsCard>

      <SettingsCard id="health" title="Health" description="How often the gateway checks the model, and who hears about it.">
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
      </SettingsCard>
    </>
  );
}

/** The Capacity card's "3 of 16 slots in use right now", read live in this leaf. */
function CapacityNow({ model }: { model: ModelRoute }) {
  const { load } = usePoolLoad(model);
  if (!model.enabled) return <>Off: takes no requests.</>;
  return <>{load.cap > 0 ? `${load.inFlight} of ${load.cap} slots in use${load.queued ? `, ${load.queued} waiting` : ""} right now.` : `${load.inFlight} in flight, no cap.`}</>;
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
              <td className="whitespace-nowrap py-1.5 pl-3 pr-3 tabular-nums text-muted-foreground">{dateTime(c.checked_at)}</td>
              <td className={cn("py-1.5 pr-3", c.status === "unhealthy" && "font-semibold")}>{checkStatusLabel(c.status)}{c.trigger !== "scheduled" ? <span className="text-muted-foreground"> · {c.trigger}</span> : null}</td>
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

/** The session affinity picker's value for a model's stored setting. */
function sessionAffinityMode(setting: ModelRoute["session_affinity"]): string {
  if (setting?.enabled === true) return "on";
  if (setting?.enabled === false) return "off";
  return "gateway";
}

function optionalNumber(n: number | undefined): string {
  return n == null ? "" : String(n);
}
