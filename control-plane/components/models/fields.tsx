"use client";

import { createContext, useContext, useEffect, useState, type ChangeEvent, type ReactNode } from "react";
import { Check, ChevronDown } from "lucide-react";
import { HelpTip } from "@/components/fairshare/help";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import type { BoonBlockers } from "@/lib/boon-availability";
import { distinctEmbeddingModelCount } from "@/lib/knowledge-format";
import { MAX_VARIANTS, touches, variantDrafts, variantNameProblem, variantsValue, type FormSnapshot, type VariantDraft } from "@/lib/models-model";
import type { KnowledgeCollection, McpServer, ModelKnowledgeCollections, ModelRoute } from "@/lib/obleth";
import { cn, parseTagLevel, TAG_LEVEL_LABELS } from "@/lib/utils";

// Fixed routing-tag vocabulary; mirrors obleth-config `MODEL_TAGS`. Used by the
// `auto` router to match requests to models.
export const MODEL_TAGS = ["coding", "general", "reasoning", "math", "vision", "long-context", "fast", "creative", "writing"] as const;

// Fixed boon vocabulary; mirrors obleth-config `MODEL_BOONS`. A boon grants a
// capability the model lacks natively. Each boon is configured globally in
// Settings → Boons, then enabled per model here. Nothing is granted by default.
export const MODEL_BOONS = [
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
  {
    value: "compression",
    label: "Compression",
    description:
      "Reduce the input tokens this model reads before dispatch: lossless JSON/code compaction always; cross-turn dedup and lossy text compaction when the tenant opts in (works on any model). If the model supports function calling with the gateway tool loop enabled, a retrieve_original tool is added so it can recover compacted detail. Configure globally in Settings → Boons and per tenant on the tenant's Compression tab.",
  },
  {
    value: "knowledge",
    label: "Knowledge",
    description:
      "Retrieve from administrator-curated collections and inject the result into the request before dispatch, at request time — the injected text is never user-supplied. Attach collections to this model below; granting this boon without attaching a collection retrieves nothing. Configure retrieval on the Knowledge page, under Retrieval settings.",
  },
  {
    value: "image_generation",
    label: "Image generation",
    description:
      "Add a generate_image tool this model can call to produce pictures through the image model configured in Settings → Boons. The gateway runs the generation and attaches the result to the reply; the image is billed per image against the caller's tenant. Requires the Function calling capability — without it no tool is injected and the model will say it cannot draw.",
  },
  {
    value: "speculation",
    label: "Speculation",
    description:
      "Answer with the configured fast drafter model whenever this model itself verifies the draft (one cheap prompt_logprobs prefill scores every draft token); drafts that fail verification fall through to this model unchanged. Same quality, several times faster on verified requests. Configure the drafter, verifier, gates, and per-category rules in Settings → Boons.",
  },
] as const;

// Model modality vocabulary; mirrors obleth-config `MODEL_TYPES`. The type
// determines which OpenAI endpoint the route serves and how it is billed.
export const MODEL_TYPE_OPTIONS = [
  { value: "chat", label: "Chat / completions" },
  { value: "embedding", label: "Embeddings" },
  { value: "audio_transcription", label: "Audio transcription (STT)" },
  { value: "audio_speech", label: "Text to speech (TTS)" },
  { value: "image", label: "Image generation" },
  { value: "video", label: "Video generation" },
  { value: "search", label: "Web search (SearXNG)" },
] as const;

// Serving-format vocabulary; mirrors obleth-config `QUANTIZATIONS`. A
// description of the deployment, not part of the model's identity, so the
// format does not have to be spelled into the API model name, where
// re-quantizing would break every pinned client.
export const QUANTIZATION_OPTIONS = [
  { value: "unknown", label: "Not declared" },
  { value: "none", label: "None (full precision)" },
  { value: "fp16", label: "FP16" },
  { value: "bf16", label: "BF16" },
  { value: "fp8", label: "FP8" },
  { value: "nvfp4", label: "NVFP4" },
  { value: "mxfp4", label: "MXFP4" },
  { value: "int8", label: "INT8" },
  { value: "int4", label: "INT4" },
  { value: "awq", label: "AWQ" },
  { value: "gptq", label: "GPTQ" },
  { value: "gguf", label: "GGUF" },
] as const;

export const QUANTIZATION_HINT =
  "Reported on /v1/models and /model/info. Keep it out of the API model name: a name like `glm-5-3-fp8` has to change when the deployment is re-quantized, and every client pinned to it breaks.";

export const UPSTREAM_HEADERS_HINT =
  "One `Name: value` per line, sent on every request to this model's upstream and overriding a client header of the same name (e.g. a routing hint an inference gateway reads, or a tenant or organization header a provider requires). Values are write-only: saved headers show by name, and a name left without a value keeps its stored value. Delete a line to remove that header. Authorization, Host, Content-Length, Content-Type, and hop-by-hop headers cannot be set; use the API key for upstream auth.";

export const ALIASES_HINT =
  "One name per line. Extra names that resolve to this same route — register the old spelling here when you clean up an API model name, and pinned clients keep working. Only the API model name itself is advertised by /v1/models.";

export const VARIANTS_HINT =
  "A variant is another name for this model with extra boons turned on: `glm-5-3-spec` could be this model with Speculation. It reaches the same deployment and shares its capacity and price. Its boons are added to the model's own and never turn one off, so callers opt in by asking for the variant while the plain name keeps working as before. A boon that is off in Settings → Boons does nothing here either.";

export function modelTypeHint(type: string): string {
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
      return "Serves /v1/images/generations, /v1/images/edits and /v1/images/variations (multipart image upload). Billed per image.";
    case "video":
      return "Serves the /v1/videos job API: create (JSON or multipart reference image), poll, download, delete, list. Billed a flat price per created job; polls and downloads are free. Health is catalog-only.";
    case "search":
      return "A web search tool, not a model: serves POST /v1/search (the Perplexity Search API shape, as LiteLLM does) and is listed on GET /v1/search/tools, not /v1/models. The API base is a SearXNG instance's root, with JSON output enabled in its settings. Searches are free.";
    default:
      return "";
  }
}

// ---------------------------------------------------------------------------
// Change highlighting
// ---------------------------------------------------------------------------

/** What the surrounding settings form has changed, so each setting can mark itself. */
export const ChangesContext = createContext<{ changed: string[]; initial: FormSnapshot | null }>({ changed: [], initial: null });

function wasLabel(initial: FormSnapshot | null, field: string, checkbox: boolean): string | null {
  if (!initial) return null;
  const value = initial.get(field);
  if (checkbox) return value === "on" ? "was on" : "was off";
  if (value == null) return null;
  return value === "" ? "was blank" : `was ${value.length > 28 ? `${value.slice(0, 27)}…` : value}`;
}

/**
 * One setting: its name and a short line on the left, the control on the
 * right. `fields` names the form fields it owns (a trailing `_` matches a
 * prefix); when any of them differs from what was loaded the row is marked,
 * and a single plain field says what it was.
 */
export function Setting({
  id,
  label,
  hint,
  help,
  fields = [],
  was,
  children,
  className,
}: {
  id?: string;
  label: string;
  hint?: ReactNode;
  help?: ReactNode;
  fields?: string[];
  /** Show "was …" for this field (or "was on/off" with `checkbox`). */
  was?: { field: string; checkbox?: boolean };
  children: ReactNode;
  className?: string;
}) {
  const { changed, initial } = useContext(ChangesContext);
  const dirty = fields.length > 0 && touches(changed, fields);
  const before = dirty && was ? wasLabel(initial, was.field, !!was.checkbox) : null;
  return (
    <div
      id={id}
      data-changed={dirty || undefined}
      className={cn(
        "relative grid scroll-mt-24 gap-x-6 gap-y-2 border-t border-border px-[18px] py-3.5 first:border-t-0 md:grid-cols-[220px_minmax(0,1fr)]",
        dirty && "bg-muted/40 before:absolute before:inset-y-2.5 before:left-0 before:w-0.5 before:rounded-full before:bg-foreground",
        className,
      )}
    >
      <div className="min-w-0">
        <div className="flex items-center gap-1.5 text-[13px] font-medium">
          {label}
          {help && <HelpTip label={`About ${label.toLowerCase()}`}>{help}</HelpTip>}
        </div>
        {hint && <p className="mt-0.5 text-xs leading-snug text-muted-foreground">{hint}</p>}
      </div>
      <div className="flex min-w-0 flex-col gap-2">
        {children}
        {before && <span className="text-[11.5px] text-muted-foreground">{before}</span>}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

/** A text or number input that submits with the surrounding form. */
export function TextField({
  name,
  label,
  className,
  mono,
  ...props
}: Omit<React.ComponentProps<typeof Input>, "name"> & { name: string; label: string; mono?: boolean }) {
  return <Input name={name} aria-label={label} className={cn("h-9 text-[13px]", mono && "font-mono text-[12.5px]", className)} {...props} />;
}

/** A labelled input stacked over its hint: for sheets and dialogs, where there is no Setting row. */
export function Field({
  label,
  name,
  hint,
  className,
  ...props
}: Omit<React.ComponentProps<typeof Input>, "name"> & { label: string; name: string; hint?: ReactNode }) {
  const id = `field-${name}`;
  return (
    <div className={cn("space-y-1.5", className)}>
      <label htmlFor={id} className="text-[12.5px] font-medium text-secondary-foreground">{label}</label>
      <Input id={id} name={name} className="h-9 text-[13px]" {...props} />
      {hint && <p className="text-[11.5px] leading-snug text-muted-foreground">{hint}</p>}
    </div>
  );
}

export function TextArea({ name, label, rows = 3, defaultValue, value, onChange, placeholder, className }: {
  name: string; label: string; rows?: number; defaultValue?: string; placeholder?: string; className?: string;
  /** Controlled when given. */
  value?: string;
  onChange?: (value: string) => void;
}) {
  return (
    <textarea
      name={name}
      aria-label={label}
      rows={rows}
      {...(value !== undefined ? { value, onChange: (e: ChangeEvent<HTMLTextAreaElement>) => onChange?.(e.target.value) } : { defaultValue })}
      placeholder={placeholder}
      autoCapitalize="none"
      autoCorrect="off"
      spellCheck={false}
      className={cn(
        "flex w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-[12.5px] shadow-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring",
        className,
      )}
    />
  );
}

/**
 * An on/off switch that is a real checkbox underneath, so it submits `on`
 * with the form. Controlled when `checked` is given. With `children`, the
 * whole card is the click target.
 */
export function Switch({
  name,
  label,
  checked,
  defaultChecked,
  onChange,
  disabled,
  children,
  className,
}: {
  name?: string;
  label: string;
  checked?: boolean;
  defaultChecked?: boolean;
  onChange?: (checked: boolean) => void;
  disabled?: boolean;
  children?: ReactNode;
  className?: string;
}) {
  const controlled = checked !== undefined;
  const track = (
    <span className="relative inline-flex h-5 w-9 shrink-0 items-center">
      <input
        type="checkbox"
        role="switch"
        name={name}
        aria-label={children ? undefined : label}
        disabled={disabled}
        className="peer sr-only"
        {...(controlled
          ? { checked, onChange: (e: ChangeEvent<HTMLInputElement>) => onChange?.(e.target.checked) }
          : { defaultChecked, onChange: onChange ? (e: ChangeEvent<HTMLInputElement>) => onChange(e.target.checked) : undefined })}
      />
      <span className="absolute inset-0 rounded-full bg-muted transition-colors peer-checked:bg-foreground peer-focus-visible:ring-2 peer-focus-visible:ring-ring peer-disabled:opacity-40" />
      <span className="absolute left-[3px] h-3.5 w-3.5 rounded-full bg-muted-foreground transition-transform peer-checked:translate-x-4 peer-checked:bg-background" />
    </span>
  );
  if (!children) {
    return <label className={cn("inline-flex", disabled ? "cursor-not-allowed" : "cursor-pointer", className)}>{track}</label>;
  }
  return (
    <label
      className={cn(
        "flex min-h-10 items-center justify-between gap-3 rounded-lg border border-border px-3 py-2 text-[13px] transition-colors hover:border-muted-foreground/60",
        disabled ? "cursor-not-allowed opacity-60" : "cursor-pointer",
        className,
      )}
    >
      <span className="min-w-0">{children}</span>
      {track}
    </label>
  );
}

// Checkbox dressed as a selectable chip. Keeps native form semantics (the
// hidden input still submits) while reading as a tag picker. Controlled with
// `checked` + `onChange` for fields whose state drives other fields.
export function ChipCheckbox({
  name,
  label,
  defaultChecked,
  checked,
  onChange,
  disabled,
  hint,
}: {
  /** Omit for a chip whose value the surrounding form carries some other way. */
  name?: string;
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
          "inline-flex h-7 items-center gap-1.5 rounded-full border border-border px-2.5 text-xs font-medium text-muted-foreground transition-colors",
          "hover:text-foreground",
          "peer-checked:border-foreground/70 peer-checked:bg-secondary peer-checked:text-foreground",
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

export function SelectField({ name, label, value, onChange, options }: {
  /** Omit for a picker whose value the surrounding form carries some other way. */
  name?: string;
  label: string;
  value: string;
  onChange?: (value: string) => void;
  options: readonly { value: string; label: string; hint?: string }[];
}) {
  return <Select name={name} aria-label={label} value={value} onValueChange={(next) => onChange?.(next)} options={options} className="h-9 text-[13px]" />;
}

// Auto / Basic / Strong / Best for a checked routing tag. Native radios whose
// group name carries the tag, so `tagsFromForm` on the server can fold
// `tag_level_<tag>` back into the stored `base:level` string.
export function TagLevelPicker({ tag, level, onChange }: { tag: string; level: number; onChange: (level: number) => void }) {
  return (
    <span role="radiogroup" aria-label={`${tag} strength level`} className="inline-flex items-center gap-0.5 rounded-lg border border-border p-[3px]">
      {([0, 1, 2, 3] as const).map((lvl) => (
        <label key={lvl} className="cursor-pointer">
          <input
            type="radio"
            name={`tag_level_${tag}`}
            value={lvl}
            checked={level === lvl}
            onChange={() => onChange(lvl)}
            aria-label={lvl === 0 ? `${tag}: Auto (level derives from cost rank)` : `${tag}: ${TAG_LEVEL_LABELS[lvl]} (level ${lvl})`}
            className="peer sr-only"
          />
          <span className="inline-flex h-6 items-center rounded-md px-2 text-[11.5px] text-muted-foreground transition-colors hover:text-foreground peer-checked:bg-secondary peer-checked:text-foreground peer-focus-visible:ring-1 peer-focus-visible:ring-ring">
            {TAG_LEVEL_LABELS[lvl]}
          </span>
        </label>
      ))}
    </span>
  );
}

// ---------------------------------------------------------------------------
// Variants
// ---------------------------------------------------------------------------

const BOON_ORDER = MODEL_BOONS.map((b) => b.value);

function listed(words: string[]): string {
  return words.length < 2 ? words.join("") : `${words.slice(0, -1).join(", ")} and ${words[words.length - 1]}`;
}

/**
 * A model's variants: each row a name, what callers get, and the boons it
 * adds. The rows submit together as one JSON field, `variants`, so the save
 * bar sees any edit as one change and the save sends the whole list.
 */
export function VariantsField({ model, modelNames = [] }: { model: ModelRoute; modelNames?: string[] }) {
  const [rows, setRows] = useState<VariantDraft[]>(() => variantDrafts(model));
  const own = { name: model.model_name, aliases: model.aliases ?? [], otherModels: modelNames.filter((n) => n !== model.model_name) };
  const edit = (i: number, patch: (row: VariantDraft) => Partial<VariantDraft>) =>
    setRows((prev) => prev.map((r, j) => (j === i ? { ...r, ...patch(r) } : r)));
  return (
    <div className="flex flex-col gap-2">
      <input type="hidden" name="variants" value={variantsValue(rows, BOON_ORDER)} />
      {rows.map((v, i) => {
        const name = v.name.trim();
        const problem = variantNameProblem(rows, i, own);
        const added = MODEL_BOONS.filter((b) => v.boons.includes(b.value)).map((b) => b.label);
        return (
          <div key={i} className="flex flex-col gap-2 rounded-lg border border-border p-3">
            <div className="flex flex-wrap items-center gap-2">
              <Input
                aria-label="Variant name"
                value={v.name}
                onChange={(e) => edit(i, () => ({ name: e.target.value }))}
                ref={(el) => el?.setCustomValidity(problem ?? "")}
                aria-invalid={problem ? true : undefined}
                required
                placeholder={`${model.model_name}-spec`}
                autoComplete="off"
                spellCheck={false}
                className="h-9 w-56 font-mono text-[12.5px]"
              />
              <Input
                aria-label="Variant description"
                value={v.description}
                onChange={(e) => edit(i, () => ({ description: e.target.value }))}
                placeholder="What callers get (optional)"
                className="h-9 min-w-0 flex-1 text-[13px]"
              />
              <button type="button" onClick={() => setRows((prev) => prev.filter((_, j) => j !== i))} aria-label={`Remove ${name || "this variant"}`} className="text-xs text-muted-foreground underline underline-offset-[3px] hover:text-foreground">Remove</button>
            </div>
            <div role="group" aria-label={`Boons ${name || "this variant"} adds`} className="flex flex-wrap gap-1.5">
              {MODEL_BOONS.map((boon) => (
                <ChipCheckbox
                  key={boon.value}
                  label={boon.label}
                  hint={boon.description}
                  checked={v.boons.includes(boon.value)}
                  onChange={(on) => edit(i, (r) => ({ boons: on ? [...r.boons, boon.value] : r.boons.filter((b) => b !== boon.value) }))}
                />
              ))}
            </div>
            {problem ? (
              <p className="text-[11.5px] text-foreground">{problem}</p>
            ) : (
              <p className="text-[11.5px] text-muted-foreground">
                {name ? <span className="font-mono">{name}</span> : "This name"} reaches {model.model_name}
                {added.length ? ` with ${listed(added)} turned on as well.` : " with no extra boons, the same as an alias."}
              </p>
            )}
          </div>
        );
      })}
      <button
        type="button"
        disabled={rows.length >= MAX_VARIANTS}
        onClick={() => setRows((prev) => [...prev, { name: "", description: "", boons: [] }])}
        className="self-start text-[12.5px] text-secondary-foreground underline underline-offset-[3px] hover:text-foreground disabled:no-underline disabled:opacity-50"
      >
        {rows.length ? "Add another variant" : "Add a variant"}
      </button>
      {rows.length >= MAX_VARIANTS && <p className="text-[11.5px] text-muted-foreground">A model can have up to {MAX_VARIANTS} variants.</p>}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Routing tags and chat capabilities
// ---------------------------------------------------------------------------

function initialTags(model?: ModelRoute) {
  const state: Record<string, { checked: boolean; level: number }> = {};
  for (const tag of MODEL_TAGS) {
    const match = model?.tags?.map(parseTagLevel).find((t) => t.base === tag);
    const nativeVision = tag === "vision" && Boolean(model?.supports_vision);
    // A bare saved tag means "Auto": the level derives from cost rank under
    // hybrid tier sourcing. Only an explicit :level suffix pins a level.
    state[tag] = { checked: Boolean(match) || nativeVision, level: match ? (match.declared ? match.level : 0) : 0 };
  }
  return state;
}

/**
 * What the auto router matches this model to, each tag with how strong the
 * model is at it. Carries `has_tags` so the save applies the set even when
 * every tag is cleared.
 */
export function RoutingTagsField({ model }: { model?: ModelRoute }) {
  const [tags, setTags] = useState(() => initialTags(model));
  return (
    <div className="flex flex-col gap-2">
      <input type="hidden" name="has_tags" value="1" />
      <div className="flex flex-wrap gap-1.5">
        {MODEL_TAGS.map((tag) => (
          <ChipCheckbox
            key={tag}
            name={`tag_${tag}`}
            label={tag}
            checked={tags[tag].checked}
            onChange={(checked) => setTags((prev) => ({ ...prev, [tag]: { ...prev[tag], checked } }))}
          />
        ))}
      </div>
      {MODEL_TAGS.some((t) => tags[t].checked) && (
        <div className="grid gap-1.5 pt-1">
          {MODEL_TAGS.filter((t) => tags[t].checked).map((tag) => (
            <div key={tag} className="flex items-center gap-3">
              <span className="w-28 truncate text-[12.5px] text-secondary-foreground">{tag}</span>
              <TagLevelPicker tag={tag} level={tags[tag].level} onChange={(level) => setTags((prev) => ({ ...prev, [tag]: { ...prev[tag], level } }))} />
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// Native capabilities, boons, and MCP tool grants for chat routes. Tool grants
// depend on native function calling + tool choice: without them the gateway
// can't run the tool loop and silently drops the tools (the model then claims
// it can't search). So the tool servers are disabled until both are on, and
// any grants are cleared the moment either is turned off. Carries
// `has_capabilities` so a save with every switch off still applies.
export function ChatCapabilityFields({
  model,
  mcpServers,
  modelNames = [],
  selfName = "",
  boonBlockers = {},
  layout = "stack",
}: {
  model?: ModelRoute;
  mcpServers: McpServer[];
  modelNames?: string[];
  selfName?: string;
  boonBlockers?: BoonBlockers;
  /** `rows` renders each group as a Setting row, for the model page. */
  layout?: "stack" | "rows";
}) {
  const [fnCalling, setFnCalling] = useState(model?.supports_function_calling ?? false);
  const [toolChoice, setToolChoice] = useState(model?.supports_tool_choice ?? false);
  const [granted, setGranted] = useState<Set<string>>(() => new Set(model?.tool_servers ?? []));
  // The knowledge and speculation boons have follow-up fields, so their
  // switches are controlled; the others stay uncontrolled.
  const [knowledgeChecked, setKnowledgeChecked] = useState(model?.boons?.includes("knowledge") ?? false);
  const [speculationChecked, setSpeculationChecked] = useState(model?.boons?.includes("speculation") ?? false);
  const [draftModel, setDraftModel] = useState(model?.draft_model ?? "");
  const [specWiringOpen, setSpecWiringOpen] = useState(false);
  const toolsReady = fnCalling && toolChoice;
  // Boons whose global switch (or helper model) is missing in Settings. The
  // grant alone does nothing in that state, so the form says so rather than
  // letting the misconfiguration surface later as "the model says it can't".
  const blockedBoons = MODEL_BOONS.flatMap((boon) => {
    const reason = boonBlockers[boon.value];
    if (!reason) return [];
    return [{ value: boon.value, label: boon.label, reason, held: model?.boons?.includes(boon.value) ?? false }];
  });

  useEffect(() => {
    if (!toolsReady) setGranted((prev) => (prev.size === 0 ? prev : new Set()));
  }, [toolsReady]);

  const native = (
    <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-4">
      <Switch name="supports_function_calling" label="Function calling" checked={fnCalling} onChange={setFnCalling}>Function calling</Switch>
      <Switch name="supports_tool_choice" label="Tool choice" checked={toolChoice} onChange={setToolChoice}>Tool choice</Switch>
      <Switch name="supports_response_schema" label="Response schema" defaultChecked={model?.supports_response_schema ?? false}>Response schema</Switch>
      <Switch name="supports_system_messages" label="System messages" defaultChecked={model ? model.supports_system_messages : true}>System messages</Switch>
    </div>
  );

  const boons = (
    <div className="flex flex-col gap-2">
      <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">
        {MODEL_BOONS.map((boon) => {
          const blocked = boonBlockers[boon.value];
          const held = model?.boons?.includes(boon.value) ?? false;
          // A boon already granted keeps its operable switch even while its
          // global switch is off: the switch is how an admin ungrants it, and
          // a disabled checkbox submits nothing — which would silently revoke
          // the grant on the next unrelated save. Only a NEW grant is refused,
          // since it would be inert the moment it was made.
          const disabled = Boolean(blocked) && !held;
          const control =
            boon.value === "knowledge" ? { checked: knowledgeChecked, onChange: setKnowledgeChecked }
            : boon.value === "speculation" ? { checked: speculationChecked, onChange: setSpeculationChecked }
            : { defaultChecked: held };
          return (
            <div key={boon.value} className="flex items-center gap-2">
              <Switch name={`boon_${boon.value}`} label={boon.label} disabled={disabled} className="flex-1" {...control}>
                <span className="block">{boon.label}</span>
                {blocked && <span className="block text-[11px] text-muted-foreground">{held ? "on here · off in Settings" : "off in Settings"}</span>}
              </Switch>
              <HelpTip label={`About ${boon.label}`} align="right">{boon.description}</HelpTip>
            </div>
          );
        })}
      </div>
      {blockedBoons.length > 0 && (
        <ul className="max-w-prose space-y-0.5 text-[11.5px] leading-snug text-muted-foreground">
          {blockedBoons.map((boon) => (
            <li key={boon.value}>
              <span className="font-medium text-foreground">{boon.label}</span>
              {boon.held ? " is granted but inactive — " : " can’t be granted — "}
              {boon.reason}
            </li>
          ))}
        </ul>
      )}
      {speculationChecked && (
        <div className="space-y-3 rounded-lg border border-border p-3">
          <div>
            <p className="text-xs font-medium">Speculation &mdash; this model&apos;s own cascade</p>
            <p className="mt-0.5 max-w-prose text-[11.5px] leading-snug text-muted-foreground">
              A fast drafter writes the answer and a scoring deployment of <span className="font-medium text-foreground">this model</span> verifies every token before anything reaches the client. Unverified drafts fall through to the model itself.
            </p>
          </div>
          <div className="max-w-sm space-y-1">
            <p className="text-[11.5px] font-medium text-muted-foreground">Drafter</p>
            <input type="hidden" name="draft_model" value={draftModel} />
            <Select
              aria-label="Drafter"
              value={draftModel}
              onValueChange={setDraftModel}
              searchPlaceholder="Filter models"
              options={[{ value: "", label: "Fleet default (Settings → Boons)" }, ...modelNames.filter((n) => n !== selfName).map((n) => ({ value: n, label: n }))]}
            />
            <p className="text-[11.5px] leading-snug text-muted-foreground">A small model 5-10x faster than this one. This model&apos;s own backend scores every draft.</p>
          </div>
          <div>
            <button
              type="button"
              onClick={() => setSpecWiringOpen((v) => !v)}
              aria-expanded={specWiringOpen}
              className="flex items-center gap-1.5 text-[11.5px] font-medium text-muted-foreground transition-colors hover:text-foreground"
            >
              <ChevronDown className={cn("h-3 w-3 transition-transform duration-200", specWiringOpen && "rotate-180")} />
              Advanced wiring — scoring endpoint override
            </button>
            {/* The fields stay in the form when collapsed so a save never
                silently clears a configured override. */}
            <div className={cn("mt-2 grid gap-3 sm:grid-cols-2", !specWiringOpen && "hidden")}>
              <Field
                label="Scoring endpoint URL"
                name="verify_api_base"
                defaultValue={model?.verify_api_base ?? ""}
                placeholder="fleet rule (Settings → Boons)"
                hint="Only when this model's drafts must be scored somewhere other than the fleet rule's address: a direct URL whose backend supports prompt_logprobs."
              />
              <Field
                label="Scoring endpoint serves (optional)"
                name="verify_upstream_model"
                defaultValue={model?.verify_upstream_model ?? ""}
                placeholder="same as upstream model"
                hint="Only if the scoring backend serves a different name than this model's upstream."
              />
            </div>
          </div>
        </div>
      )}
      {knowledgeChecked &&
        (model ? (
          <ModelKnowledgeCollectionsField modelId={model.id} />
        ) : (
          <p className="max-w-prose text-[11.5px] leading-snug text-muted-foreground">
            Attach collections from the model&apos;s page once it exists — the knowledge boon retrieves nothing until at least one collection is attached.
          </p>
        ))}
    </div>
  );

  const tools =
    mcpServers.length === 0 ? (
      <p className="text-xs text-muted-foreground">No MCP servers registered. Add one on the MCP page first.</p>
    ) : !toolsReady ? (
      <p className="max-w-prose text-[11.5px] leading-snug text-muted-foreground">
        Turn on <span className="font-medium text-foreground">Function calling</span> and <span className="font-medium text-foreground">Tool choice</span> to grant tools — the gateway’s tool loop can’t run without them.
      </p>
    ) : (
      <div className="flex flex-wrap gap-1.5">
        {mcpServers.map((server) => (
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
        ))}
      </div>
    );

  const marker = <input type="hidden" name="has_capabilities" value="1" />;
  if (layout === "rows") {
    return (
      <>
        <Setting id="set-native" label="Native" hint="What the model does itself. Gates request features and routing." fields={["supports_"]}>
          {marker}
          {native}
        </Setting>
        <Setting id="set-boons" label="Boons" hint="Gateway features that fill in what the model lacks." fields={["boon_", "draft_model", "verify_api_base", "verify_upstream_model", "knowledge_collection"]}>
          {boons}
        </Setting>
        <Setting id="set-tools" label="Tool servers" hint="MCP servers whose tools the gateway's tool loop may call for this model." fields={["tool_server_"]}>
          {tools}
        </Setting>
      </>
    );
  }
  return (
    <div className="space-y-4">
      {marker}
      <Group label="Native capabilities">{native}</Group>
      <Group label="Boons">{boons}</Group>
      <Group label="Tools">{tools}</Group>
    </div>
  );
}

function Group({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="space-y-2">
      <p className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">{label}</p>
      {children}
    </div>
  );
}

// Attaches administrator-curated knowledge collections to a chat model once
// the `knowledge` boon is granted. The selection submits with the settings
// form as `knowledge_collection` values and saves with everything else.
//
// The current attachment set is read back first. If that read fails, the
// selection has no reliable relationship to what is attached: an empty list
// would look like "nothing attached yet", and saving it would silently detach
// whatever is really there. So `knowledge_loaded` — which the save needs
// before it touches attachments — is only sent once the read succeeds.
export function ModelKnowledgeCollectionsField({ modelId }: { modelId: string }) {
  const [collections, setCollections] = useState<KnowledgeCollection[] | null>(null);
  const [loadError, setLoadError] = useState(false);
  const [attachments, setAttachments] = useState<"loading" | "loaded" | "failed">("loading");
  const [selected, setSelected] = useState<Set<string>>(new Set());

  useEffect(() => {
    let cancelled = false;
    fetch("/api/live/knowledge/collections")
      .then((res) => (res.ok ? res.json() : Promise.reject(res)))
      .then((data: KnowledgeCollection[]) => { if (!cancelled) setCollections(data); })
      .catch(() => { if (!cancelled) setLoadError(true); });
    fetch(`/api/live/models/${modelId}/knowledge`)
      .then((res) => (res.ok ? res.json() : Promise.reject(res)))
      .then((data: ModelKnowledgeCollections) => {
        if (cancelled) return;
        setSelected(new Set(data.collection_ids));
        setAttachments("loaded");
      })
      .catch(() => { if (!cancelled) setAttachments("failed"); });
    return () => { cancelled = true; };
  }, [modelId]);

  const selectedCollections = (collections ?? []).filter((c) => selected.has(c.id));
  const distinctEmbedderCount = distinctEmbeddingModelCount(selectedCollections);

  return (
    <div className="space-y-2 rounded-lg border border-border p-3">
      <p className="text-xs font-medium">Attached collections</p>
      {attachments === "loaded" && (
        <>
          <input type="hidden" name="knowledge_loaded" value="1" />
          {[...selected].sort().map((id) => <input key={id} type="hidden" name="knowledge_collection" value={id} />)}
        </>
      )}
      {attachments === "failed" && (
        <p className="max-w-prose text-[11.5px] leading-snug text-foreground">
          Could not load this model&apos;s current attachments, so they are left as they are when you save.
        </p>
      )}
      {loadError ? (
        <p className="text-xs text-foreground">Failed to load collections.</p>
      ) : collections === null || attachments === "loading" ? (
        <p className="text-xs text-muted-foreground">Loading collections…</p>
      ) : collections.length === 0 ? (
        <p className="text-xs text-muted-foreground">No collections exist yet. Create one on the Knowledge page.</p>
      ) : (
        <div className="flex flex-wrap gap-1.5">
          {collections.map((c) => {
            const on = selected.has(c.id);
            return (
              <button
                key={c.id}
                type="button"
                aria-pressed={on}
                disabled={attachments !== "loaded"}
                onClick={() => setSelected((prev) => { const next = new Set(prev); if (on) next.delete(c.id); else next.add(c.id); return next; })}
                title={`Embedded with ${c.indexed_embedding_model || c.embedding_model}`}
                className={cn(
                  "inline-flex h-7 items-center gap-1.5 rounded-full border border-border px-2.5 text-xs font-medium text-muted-foreground transition-colors hover:text-foreground disabled:opacity-50",
                  on && "border-foreground/70 bg-secondary text-foreground",
                )}
              >
                <Check className={cn("h-3 w-3", !on && "hidden")} strokeWidth={2.5} />
                {c.name}
              </button>
            );
          })}
        </div>
      )}
      {distinctEmbedderCount > 1 && (
        <p className="max-w-prose text-[11.5px] leading-snug text-muted-foreground">
          These collections use {distinctEmbedderCount} different embedding models — each distinct embedder adds a network round trip per request.
        </p>
      )}
      {attachments === "loaded" && selected.size === 0 && (
        <p className="max-w-prose text-[11.5px] leading-snug text-muted-foreground">No collection attached — the knowledge boon retrieves nothing until one is.</p>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Numbers
// ---------------------------------------------------------------------------

// Renders a number as a plain decimal string, never scientific notation, so
// per-token costs like 0.00000008 stay editable instead of showing as "8e-8".
// Expands the shortest round-trip representation by hand to avoid the float
// artifacts that `toFixed` introduces for tiny values.
export function toPlainDecimal(value: number): string {
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
  if (pointPos <= 0) body = `0.${"0".repeat(-pointPos)}${digits}`;
  else if (pointPos >= digits.length) body = digits + "0".repeat(pointPos - digits.length);
  else body = `${digits.slice(0, pointPos)}.${digits.slice(pointPos)}`;
  return (negative ? "-" : "") + body;
}

/** A per-token price as a per-million figure for editing: 0.0000006 → "0.6". */
export function perMillion(costPerToken: number): string {
  if (!Number.isFinite(costPerToken) || costPerToken <= 0) return "";
  return toPlainDecimal(Number((costPerToken * 1_000_000).toPrecision(12)));
}

/** Back from a per-million figure to the per-token value the gateway stores. */
export function fromPerMillion(value: string): string {
  const n = Number(value.trim());
  if (value.trim() === "" || !Number.isFinite(n)) return "";
  return toPlainDecimal(Number((n / 1_000_000).toPrecision(12)));
}
