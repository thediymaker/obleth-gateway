"use client";

import { useMemo, useRef, useState, useTransition, type ChangeEvent } from "react";
import { useRouter } from "next/navigation";
import { ChevronDown, Upload } from "lucide-react";
import { applyModelManifestAction, createModelAction, listUpstreamModelsAction } from "@/app/actions";
import { ManifestPreview, ManifestResultBanner } from "@/components/model-import-review";
import { Field, fromPerMillion, MODEL_TYPE_OPTIONS, SelectField, TextArea, UPSTREAM_HEADERS_HINT } from "@/components/models/fields";
import { Notice, Sheet } from "@/components/models/ui";
import { Segmented } from "@/components/overview/ui";
import { ProviderImportWizard } from "@/components/provider-import-wizard";
import { RecipeList } from "@/components/recipes/recipe-list";
import type { RecipeCard } from "@/components/recipes/recipe-card";
import { HelpTip } from "@/components/fairshare/help";
import { Button } from "@/components/ui/button";
import { modelHref } from "@/lib/models-model";
import { normalizeModelApiNameDraft, normalizeModelApiNameFinal } from "@/lib/model-name";
import type { ModelImportReport, ModelRoute } from "@/lib/obleth";
import type { UpstreamModel } from "@/lib/provider-import";
import { cn } from "@/lib/utils";

export type AddMode = "connect" | "provider" | "file" | "slurm";

/** A first guess at the type from the upstream's name; only a default, shown for the admin to confirm. */
export function guessModelType(upstream: string): string {
  const s = upstream.toLowerCase();
  if (/embed|bge-|e5-|gte-/.test(s)) return "embedding";
  if (/whisper|asr|transcri|parakeet/.test(s)) return "audio_transcription";
  if (/tts|speech|kokoro|voice/.test(s)) return "audio_speech";
  if (/wan-?\d|video|hunyuanvideo|ltx|mochi/.test(s)) return "video";
  if (/flux|sdxl|stable-diffusion|diffusion|dall-?e|imagen|qwen-image/.test(s)) return "image";
  return "chat";
}

/** The API name suggested for an upstream id: its last path segment, normalized. */
export function suggestName(upstream: string): string {
  return normalizeModelApiNameFinal(upstream.split("/").pop() ?? upstream);
}

export function AddModelSheet({
  mode,
  onModeChange,
  onClose,
  slurmEnabled,
  recipeCards,
  models,
}: {
  mode: AddMode | null;
  onModeChange: (mode: AddMode) => void;
  onClose: () => void;
  slurmEnabled: boolean;
  recipeCards: RecipeCard[];
  models: ModelRoute[];
}) {
  const modes: { value: AddMode; label: string }[] = [
    { value: "connect", label: "Connect an endpoint" },
    { value: "provider", label: "From a provider" },
    { value: "file", label: "Import a file" },
    ...(slurmEnabled ? [{ value: "slurm" as const, label: "Launch on Slurm" }] : []),
  ];
  return (
    <Sheet
      open={mode !== null}
      onClose={onClose}
      title="Add a model"
      description="Point obleth at something that already serves a model. Everything else can wait for the model's own page."
      width={mode === "provider" || mode === "slurm" ? "w-[min(920px,100vw)]" : undefined}
    >
      <div className="px-6 pb-4">
        <Segmented<AddMode> label="How to add" value={mode ?? "connect"} options={modes} onChange={onModeChange} />
      </div>
      {mode === "connect" && <ConnectForm models={models} onClose={onClose} />}
      {mode === "provider" && (
        <div className="border-t border-border px-6 py-5">
          <ProviderImportWizard models={models} onClose={onClose} />
        </div>
      )}
      {mode === "file" && <ImportFile />}
      {mode === "slurm" && (
        <div className="border-t border-border px-6 py-5">
          <p className="mb-4 max-w-2xl text-[13px] text-muted-foreground">
            Pick an admin-authored <code className="font-mono">*.recipe</code> file to launch replicas on the cluster. Launching moves to Deployments once that page exists.
          </p>
          <RecipeList recipes={recipeCards} onDeployed={onClose} />
        </div>
      )}
    </Sheet>
  );
}

function Group({ step, title, children, action }: { step: number; title: string; children: React.ReactNode; action?: React.ReactNode }) {
  return (
    <section className="flex flex-col gap-3.5 border-t border-border px-6 py-5">
      <div className="flex items-center justify-between gap-3">
        <h3 className="flex items-center gap-2.5 text-[13.5px] font-semibold">
          <span className="inline-flex h-5 w-5 items-center justify-center rounded-full border border-muted-foreground/60 text-[11px] font-medium text-secondary-foreground">{step}</span>
          {title}
        </h3>
        {action}
      </div>
      {children}
    </section>
  );
}

const PRICE_FIELDS: Record<string, { name: string; label: string; perMillion: boolean }[]> = {
  chat: [
    { name: "input_cost_per_token", label: "Input per 1M tokens", perMillion: true },
    { name: "output_cost_per_token", label: "Output per 1M tokens", perMillion: true },
  ],
  embedding: [{ name: "input_cost_per_token", label: "Input per 1M tokens", perMillion: true }],
  image: [{ name: "cost_per_image", label: "Per image", perMillion: false }],
  video: [{ name: "cost_per_video", label: "Per video", perMillion: false }],
  audio_speech: [{ name: "cost_per_character", label: "Per 1M characters", perMillion: true }],
  audio_transcription: [{ name: "cost_per_audio_second", label: "Per audio second", perMillion: false }],
};

function ConnectForm({ models, onClose }: { models: ModelRoute[]; onClose: () => void }) {
  const router = useRouter();
  const form = useRef<HTMLFormElement>(null);
  const [pending, start] = useTransition();
  const [apiBase, setApiBase] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [listing, setListing] = useState<{ base: string; models: UpstreamModel[]; ms: number } | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [upstream, setUpstream] = useState("");
  const [name, setName] = useState("");
  const [nameTouched, setNameTouched] = useState(false);
  const [type, setType] = useState("chat");
  const [typeTouched, setTypeTouched] = useState(false);
  const [headersOpen, setHeadersOpen] = useState(false);
  const [prices, setPrices] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<{ name: string; enabled: boolean; check: string | null; warnings?: string[] }[]>([]);

  const taken = useMemo(() => new Set(models.flatMap((m) => [m.model_name, ...(m.aliases ?? [])])), [models]);
  const finalName = normalizeModelApiNameFinal(name);
  const nameTaken = finalName !== "" && taken.has(finalName);
  const priceFields = PRICE_FIELDS[type] ?? [];
  const textual = type === "chat" || type === "embedding";

  const pickUpstream = (id: string) => {
    setUpstream(id);
    if (!nameTouched) setName(suggestName(id));
    if (!typeTouched) setType(guessModelType(id));
  };

  function checkConnection() {
    setListError(null);
    setListing(null);
    const t0 = performance.now();
    start(async () => {
      const res = await listUpstreamModelsAction({ apiBase, apiKey: apiKey || undefined });
      if (!res.ok) {
        setListError(res.error);
        return;
      }
      setListing({ base: res.base, models: res.models, ms: Math.round(performance.now() - t0) });
      if (res.models.length === 1) pickUpstream(res.models[0].id);
    });
  }

  function reset() {
    form.current?.reset();
    setUpstream("");
    setName("");
    setNameTouched(false);
    setTypeTouched(false);
    setPrices({});
    setError(null);
  }

  function submit(another: boolean) {
    const el = form.current;
    if (!el) return;
    if (!apiBase.trim()) return setError("Add the API base URL for the endpoint.");
    if (!upstream.trim()) return setError("Pick or type the upstream model the endpoint serves.");
    if (!finalName) return setError("Give the model the name clients will send.");
    if (nameTaken) return setError(`“${finalName}” is already a model or alias.`);
    setError(null);
    const data = new FormData(el);
    data.set("model_name", finalName);
    for (const f of priceFields) data.set(f.name, f.perMillion ? fromPerMillion(prices[f.name] ?? "") : (prices[f.name] ?? "").trim());
    start(async () => {
      const result = await createModelAction(data);
      if (!result.ok) {
        setError(result.error);
        return;
      }
      if (another) {
        setCreated((c) => [{ name: result.model.name, enabled: result.enabled, check: result.check, warnings: result.warnings }, ...c]);
        reset();
        router.refresh();
      } else {
        onClose();
        router.push(modelHref(result.model.name));
      }
    });
  }

  return (
    <form ref={form} onSubmit={(e) => { e.preventDefault(); submit(false); }} className="flex flex-col">
      <input type="hidden" name="endpoint_mode" value="static" />
      <input type="hidden" name="auto_eligible" value="on" />
      <input type="hidden" name="supports_system_messages" value="on" />
      <input type="hidden" name="upstream_model" value={upstream} />
      <input type="hidden" name="model_type" value={type} />

      {created.length > 0 && (
        <div className="space-y-2 px-6 pb-4">
          {created.map((c) => (
            <Notice key={c.name} onDismiss={() => setCreated((all) => all.filter((x) => x.name !== c.name))} strong={!c.enabled}>
              <p>
                <a href={modelHref(c.name)} className="font-mono underline-offset-2 hover:underline">{c.name}</a>{" "}
                {c.enabled ? "is serving: its first health check passed." : "was created switched off."}
              </p>
              {!c.enabled && c.check && <p className="mt-0.5 text-xs text-muted-foreground">First check: {c.check}</p>}
              {c.warnings?.map((w) => <p key={w} className="mt-0.5 text-xs text-muted-foreground">{w}</p>)}
            </Notice>
          ))}
        </div>
      )}

      <Group step={1} title="Where it lives">
        <Field label="API base" name="api_base" value={apiBase} onChange={(e) => setApiBase(e.target.value)} placeholder="http://my-inference-server:8000/v1" autoComplete="off" spellCheck={false} className="[&_input]:font-mono" />
        <div className="grid items-end gap-3 sm:grid-cols-[minmax(0,1fr)_auto]">
          <Field label="API key (optional)" name="api_key" type="password" value={apiKey} onChange={(e) => setApiKey(e.target.value)} placeholder="Stored encrypted, never shown again" autoComplete="off" />
          <Button type="button" variant="outline" className="h-9" disabled={pending || !apiBase.trim()} onClick={checkConnection}>Check connection</Button>
        </div>
        {listError && <p className="rounded-lg border border-foreground/60 px-3 py-2 text-[12.5px]">{listError} You can still type the upstream model below.</p>}
        {listing && (
          <div className="rounded-lg border border-border p-1.5">
            <p className="flex justify-between px-2.5 pb-1.5 pt-1 text-xs text-muted-foreground">
              <span>Connected in {listing.ms} ms · {listing.models.length} model{listing.models.length === 1 ? "" : "s"} served</span>
              {listing.models.length > 1 && <span>pick one</span>}
            </p>
            <div role="radiogroup" aria-label="Models the endpoint serves" className="max-h-56 overflow-y-auto">
              {listing.models.map((m) => (
                <button
                  key={m.id}
                  type="button"
                  role="radio"
                  aria-checked={upstream === m.id}
                  onClick={() => pickUpstream(m.id)}
                  className={cn("flex w-full items-center gap-2.5 rounded-md px-2.5 py-1.5 text-left", upstream === m.id ? "bg-secondary" : "hover:bg-muted/50")}
                >
                  <span className={cn("h-3.5 w-3.5 shrink-0 rounded-full border", upstream === m.id ? "border-[4px] border-foreground" : "border-muted-foreground")} />
                  <span className="min-w-0 flex-1 truncate font-mono text-[12.5px]">{m.id}</span>
                  {m.owned_by && <span className="text-[11.5px] text-muted-foreground">{m.owned_by}</span>}
                </button>
              ))}
              {listing.models.length === 0 && <p className="px-2.5 py-2 text-xs text-muted-foreground">The endpoint answered but lists no models. Type the upstream model below.</p>}
            </div>
          </div>
        )}
        <Field
          label="Upstream model"
          name="upstream_display"
          value={upstream}
          onChange={(e) => pickUpstream(e.target.value)}
          placeholder="Qwen/Qwen3-8B"
          spellCheck={false}
          autoComplete="off"
          hint="The name the endpoint itself serves. Filled in when you pick one above."
          className="[&_input]:font-mono"
        />
        <div>
          <button type="button" aria-expanded={headersOpen} onClick={() => setHeadersOpen((v) => !v)} className="flex items-center gap-1.5 text-[12.5px] text-muted-foreground hover:text-foreground">
            <ChevronDown className={cn("h-3.5 w-3.5 transition-transform", headersOpen && "rotate-180")} />
            Extra upstream headers
          </button>
          <div className={cn("mt-2 space-y-1.5", !headersOpen && "hidden")}>
            <TextArea name="upstream_headers" label="Upstream headers" placeholder="x-routing-hint: sticky" />
            <p className="text-[11.5px] leading-snug text-muted-foreground">{UPSTREAM_HEADERS_HINT}</p>
          </div>
        </div>
      </Group>

      <Group step={2} title="What people call it">
        <div className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_220px]">
          <Field
            label="Name"
            name="model_name_input"
            value={name}
            onChange={(e) => { setName(normalizeModelApiNameDraft(e.target.value)); setNameTouched(true); }}
            onBlur={() => setName(normalizeModelApiNameFinal(name))}
            placeholder="qwen3-8b"
            spellCheck={false}
            autoCapitalize="none"
            autoComplete="off"
            className="[&_input]:font-mono"
            hint={nameTaken ? <span className="text-foreground">Already a model or alias.</span> : <>What clients send as <code className="font-mono">model</code>. Lowercase; spaces become dashes.</>}
          />
          <div className="space-y-1.5">
            <p className="text-[12.5px] font-medium text-secondary-foreground">Type</p>
            <SelectField label="Model type" value={type} onChange={(v) => { setType(v); setTypeTouched(true); }} options={MODEL_TYPE_OPTIONS} />
            {!typeTouched && upstream && <p className="text-[11.5px] text-muted-foreground">Guessed from the upstream name</p>}
          </div>
        </div>
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="space-y-1.5">
            <p className="text-[12.5px] font-medium text-secondary-foreground">Aliases (optional)</p>
            <TextArea name="aliases" label="Aliases" rows={2} placeholder={"old-name\nanother-name"} />
          </div>
          <div className="space-y-1.5">
            <p className="text-[12.5px] font-medium text-secondary-foreground">Description (optional)</p>
            <textarea name="description" aria-label="Description" rows={2} placeholder="What it's good for" className="flex w-full rounded-md border border-input bg-background px-3 py-2 text-[13px] shadow-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring" />
          </div>
        </div>
      </Group>

      <Group
        step={3}
        title="Limits and price"
        action={<HelpTip label="What else can be set" align="right">Router tags, capabilities, boons, health checks, discovered capacity and extra endpoints are set on the model&apos;s page once it exists.</HelpTip>}
      >
        <div className="grid gap-3 sm:grid-cols-3">
          <Field label="Concurrent requests" name="max_in_flight" type="number" min={1} placeholder="No cap" hint="Blank for no cap. Discovered capacity is set on the model's page." />
          {textual && <Field label="Context window" name="context_window" type="number" min={1} defaultValue="131072" />}
          {priceFields.map((f) => (
            <Field key={f.name} label={`${f.label} ($)`} name={`${f.name}_input`} inputMode="decimal" value={prices[f.name] ?? ""} onChange={(e) => setPrices((p) => ({ ...p, [f.name]: e.target.value }))} placeholder="0" />
          ))}
        </div>
      </Group>

      {error && <p role="alert" className="mx-6 mb-3 rounded-lg border border-foreground/60 px-3 py-2 text-[12.5px]">{error}</p>}
      <div className="sticky bottom-0 flex flex-wrap items-center justify-between gap-3 border-t border-border bg-card px-6 py-3.5">
        <span className="text-xs text-muted-foreground">Starts off, and comes on when its first health check passes</span>
        <div className="flex gap-2">
          <Button type="button" variant="outline" disabled={pending} onClick={() => submit(true)}>Create and add another</Button>
          <Button type="submit" disabled={pending}>{pending ? "Creating…" : "Create model"}</Button>
        </div>
      </div>
    </form>
  );
}

function ImportFile() {
  const input = useRef<HTMLInputElement>(null);
  const router = useRouter();
  const [pending, start] = useTransition();
  const [text, setText] = useState("");
  const [preview, setPreview] = useState<ModelImportReport | null>(null);
  const [result, setResult] = useState<ModelImportReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  function onFile(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;
    setPreview(null);
    setResult(null);
    setError(null);
    const reader = new FileReader();
    reader.onload = () => {
      const body = String(reader.result ?? "");
      start(async () => {
        // Dry run first: the gateway validates the whole file and reports the
        // per-model diff without writing anything.
        const res = await applyModelManifestAction(body, true);
        if (res.ok) {
          setText(body);
          setPreview(res.report);
        } else setError(res.error);
      });
    };
    reader.onerror = () => setError("Could not read the selected file.");
    reader.readAsText(file);
  }

  function apply() {
    start(async () => {
      const res = await applyModelManifestAction(text, false);
      setPreview(null);
      setText("");
      if (res.ok) {
        setResult(res.report);
        router.refresh();
      } else setError(res.error);
    });
  }

  return (
    <div className="flex flex-col gap-4 border-t border-border px-6 py-5">
      <p className="max-w-xl text-[13px] text-muted-foreground">
        A models file exported from obleth (JSON or YAML). You see what it would add and change before anything is written; models it adds keep the on or off state the file gives them.
      </p>
      <input ref={input} type="file" accept=".yaml,.yml,.json,text/yaml,application/json" className="hidden" onChange={onFile} />
      <div>
        <Button type="button" variant="outline" disabled={pending} onClick={() => input.current?.click()}>
          <Upload className="h-3.5 w-3.5" />
          {pending && !preview ? "Reading…" : "Choose a file"}
        </Button>
      </div>
      {error && <p role="alert" className="rounded-lg border border-foreground/60 px-3 py-2 text-[12.5px]">Import failed: {error}</p>}
      {preview && <ManifestPreview report={preview} pending={pending} onConfirm={apply} onCancel={() => { setPreview(null); setText(""); }} />}
      {result && <ManifestResultBanner report={result} onDismiss={() => setResult(null)} />}
    </div>
  );
}
