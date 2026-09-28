"use client";

import { useState, type ReactNode } from "react";
import Link from "next/link";
import { Plus, X } from "lucide-react";
import { SettingsCard } from "@/components/access/ui";
import { Glyph } from "@/components/deployments/ui";
import { SelectField, Setting, Switch, TextArea, TextField } from "@/components/models/fields";
import { Pill } from "@/components/overview/ui";
import type { BoonSettingsView, CompressorStatusView, KnowledgeSettingsView, ModelRoute, SpeculationCategoryGate } from "@/lib/obleth";
import { BOONS, boonSummary, modelsAsking, SPEC_PROFILES, specProfileOf, specValues, type BoonKey, type SpecValues } from "@/lib/settings-model";
import { cn } from "@/lib/utils";

function NumberField({ name, label, value, unit, className = "w-24" }: { name: string; label: string; value: number; unit?: string; className?: string }) {
  return (
    <span className="inline-flex items-center gap-1.5">
      <TextField name={`boons.${name}`} label={label} inputMode="decimal" defaultValue={value} mono className={className} />
      {unit && <span className="text-xs text-muted-foreground">{unit}</span>}
    </span>
  );
}

/** A speculation value, controlled so a profile can set it. */
function SpecNumber({ k, label, spec, setSpec, unit, className = "w-24" }: { k: keyof Omit<SpecValues, "gates" | "unlisted">; label: string; spec: SpecValues; setSpec: (f: (s: SpecValues) => SpecValues) => void; unit?: string; className?: string }) {
  const [text, setText] = useState<string | null>(null);
  // What's typed wins while it still means the stored value (e.g. "0.50" or a half-typed "-"); a profile's value replaces it.
  const shown = text !== null && (text.trim() === "" || text.trim() === "-" || Number(text) === spec[k]) ? text : String(spec[k]);
  return (
    <span className="inline-flex items-center gap-1.5">
      <TextField
        name={`boons.speculation_${k}`}
        label={label}
        inputMode="decimal"
        value={shown}
        onChange={(e) => {
          setText(e.target.value);
          const n = Number(e.target.value);
          if (e.target.value.trim() !== "" && Number.isFinite(n)) setSpec((x) => ({ ...x, [k]: n }));
        }}
        mono
        className={className}
      />
      {unit && <span className="text-xs text-muted-foreground">{unit}</span>}
    </span>
  );
}

function modelOptions(models: ModelRoute[], filter: (m: ModelRoute) => boolean, none: string, current?: string | null) {
  const names = [...new Set([...(current ? [current] : []), ...models.filter(filter).map((m) => m.model_name)])].sort();
  return [{ value: "", label: none }, ...names.map((n) => ({ value: n, label: n }))];
}

/** A boon's row: its switch, what it's set to, who asks for it, and its settings when opened. */
function BoonRow({ id, label, blurb, on, onToggle, summary, asking, open, onOpen, children, switchName }: {
  id: string; label: string; blurb: string; on: boolean; onToggle: (on: boolean) => void; summary: string; asking: string[]; open: boolean; onOpen: () => void; children: ReactNode; switchName: string;
}) {
  return (
    <div id={id} className="scroll-mt-24 border-t border-border">
      <div className={cn("grid min-h-[60px] grid-cols-[44px_minmax(0,1.1fr)_minmax(0,1.5fr)_minmax(0,1fr)_84px] items-center gap-3.5 px-[18px] py-2.5 text-[13px]", open && "bg-muted/30")}>
        <Switch name={switchName} label={`${label} on`} checked={on} onChange={onToggle} />
        <span className="flex min-w-0 flex-col"><span className="font-medium">{label}</span><span className="text-[11.5px] text-muted-foreground">{blurb}</span></span>
        <span className={cn("min-w-0 truncate", on ? "text-secondary-foreground" : "text-muted-foreground")} title={summary}>{summary}</span>
        <span className="flex min-w-0 flex-col items-start gap-0.5">
          {asking.length === 0 ? <span className="text-muted-foreground">none</span> : !on ? <Pill inverted>{asking.length} ask{asking.length === 1 ? "s" : ""}, off</Pill> : <span className="text-[12.5px]">{asking.length} model{asking.length === 1 ? "" : "s"}</span>}
          {asking.length > 0 && <span className="max-w-full truncate font-mono text-[11.5px] text-muted-foreground" title={asking.join(", ")}>{asking.slice(0, 3).join(", ")}{asking.length > 3 ? ` +${asking.length - 3}` : ""}</span>}
        </span>
        <button type="button" onClick={onOpen} aria-expanded={open} className={cn("inline-flex h-8 items-center justify-center rounded-lg border px-3 text-[12.5px]", open ? "border-foreground" : "border-border hover:border-muted-foreground/60")}>{open ? "Close" : "Open"}</button>
      </div>
      {/* Kept mounted when closed so every field still submits with the form. */}
      <div className={cn("border-t border-border bg-muted/30 pl-[58px]", !open && "hidden")}>{children}</div>
    </div>
  );
}

function GatesEditor({ gates, onChange }: { gates: SpeculationCategoryGate[]; onChange: (g: SpeculationCategoryGate[]) => void }) {
  const patch = (i: number, p: Partial<SpeculationCategoryGate>) => onChange(gates.map((g, j) => (j === i ? { ...g, ...p } : g)));
  const num = (v: string) => (v.trim() === "" ? undefined : Number(v));
  return (
    <div className="flex flex-col">
      <div className="grid grid-cols-[minmax(0,1fr)_100px_90px_90px_28px] gap-2 pb-1 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground"><span>Category</span><span>Try a draft</span><span>Agreement</span><span>Confidence</span><span /></div>
      {gates.map((g, i) => (
        <div key={i} className="grid grid-cols-[minmax(0,1fr)_100px_90px_90px_28px] items-center gap-2 border-t border-border py-1.5">
          <input value={g.tag} onChange={(e) => patch(i, { tag: e.target.value })} aria-label="Category" className="h-8 rounded-md border border-input bg-background px-2 font-mono text-[12px]" />
          <label className="flex items-center gap-1.5 text-[12.5px]"><input type="checkbox" checked={!!g.speculate} onChange={(e) => patch(i, { speculate: e.target.checked })} className="accent-foreground" />{g.speculate ? "yes" : "no"}</label>
          <input value={g.agree_min ?? ""} onChange={(e) => patch(i, { agree_min: num(e.target.value) })} inputMode="decimal" placeholder="global" aria-label="Agreement" className="h-8 rounded-md border border-input bg-background px-2 font-mono text-[12px]" />
          <input value={g.lp_min ?? ""} onChange={(e) => patch(i, { lp_min: num(e.target.value) })} inputMode="decimal" placeholder="global" aria-label="Confidence" className="h-8 rounded-md border border-input bg-background px-2 font-mono text-[12px]" />
          <button type="button" aria-label={`Remove ${g.tag || "this category"}`} onClick={() => onChange(gates.filter((_, j) => j !== i))} className="text-muted-foreground hover:text-foreground"><X className="h-4 w-4" /></button>
        </div>
      ))}
      <button type="button" onClick={() => onChange([...gates, { tag: "", speculate: false }])} className="mt-1.5 inline-flex items-center gap-1 self-start text-[12.5px] text-secondary-foreground hover:text-foreground"><Plus className="h-3.5 w-3.5" />Add a category</button>
    </div>
  );
}

/** Abilities the gateway adds to a model; each model opts in on its own page. */
export function BoonsSection({ settings: b, models, knowledge, compressor }: { settings: BoonSettingsView; models: ModelRoute[]; knowledge: KnowledgeSettingsView | null; compressor: CompressorStatusView | null }) {
  const [on, setOn] = useState<Record<BoonKey, boolean>>(Object.fromEntries(BOONS.map((x) => [x.key, !!b[x.enabled]])) as Record<BoonKey, boolean>);
  const [open, setOpen] = useState<BoonKey | null>(null);
  const [spec, setSpec] = useState<SpecValues>(specValues(b));
  const [picks, setPicks] = useState({
    vision_fallback_model: b.vision_fallback_model ?? "",
    structured_output_fixer_model: b.structured_output_fixer_model ?? "",
    image_generation_model: b.image_generation_model ?? "",
    speculation_draft_model: b.speculation_draft_model ?? "",
    speculation_classify_model: b.speculation_classify_model ?? "",
  });
  const [specAdvanced, setSpecAdvanced] = useState(false);
  const pick = (k: keyof typeof picks) => (v: string) => setPicks((p) => ({ ...p, [k]: v }));
  const chat = (m: ModelRoute) => m.model_type === "chat";
  const count = BOONS.filter((x) => on[x.key]).length;
  const profile = specProfileOf(spec);
  const row = (key: BoonKey, children: ReactNode) => {
    const meta = BOONS.find((x) => x.key === key)!;
    return (
      <BoonRow
        key={key}
        id={`boon-${key}`}
        label={meta.label}
        blurb={meta.blurb}
        on={on[key]}
        onToggle={(v) => setOn((o) => ({ ...o, [key]: v }))}
        summary={boonSummary(key, { ...b, ...picks, vision_fallback_model: picks.vision_fallback_model || null, image_generation_model: picks.image_generation_model || null, speculation_draft_model: picks.speculation_draft_model || null, structured_output_fixer_model: picks.structured_output_fixer_model || null, speculation_category_gates: spec.gates })}
        asking={modelsAsking(key, models)}
        open={open === key}
        onOpen={() => setOpen(open === key ? null : key)}
        switchName={`boons.${meta.enabled}`}
      >
        {children}
      </BoonRow>
    );
  };

  return (
    <SettingsCard id="boons" title="Boons" description="Abilities the gateway adds to a model. A model opts in on its own page; the switch here turns the ability on for every model that asks." action={<Pill>{count} of {BOONS.length} on</Pill>}>
      <div className="grid grid-cols-[44px_minmax(0,1.1fr)_minmax(0,1.5fr)_minmax(0,1fr)_84px] gap-3.5 px-[18px] py-2 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground"><span /><span>Boon</span><span>What it&apos;s set to</span><span>Models asking</span><span /></div>

      {row("vision", <>
        <Setting label="Describer" hint="A vision model that describes each image in words." fields={["boons.vision_fallback_model"]} was={{ field: "boons.vision_fallback_model" }}>
          <div className="w-72"><SelectField name="boons.vision_fallback_model" label="Describer model" value={picks.vision_fallback_model} onChange={pick("vision_fallback_model")} options={modelOptions(models, (m) => chat(m) && m.supports_vision, "None", b.vision_fallback_model)} /></div>
        </Setting>
        <Setting label="Limits" hint="Images described per request, and how long each may take." fields={["boons.vision_max_images", "boons.vision_timeout_ms"]}>
          <div className="flex flex-wrap gap-3"><NumberField name="vision_max_images" label="Images per request" value={b.vision_max_images} unit="images" className="w-20" /><NumberField name="vision_timeout_ms" label="Describe timeout" value={b.vision_timeout_ms} unit="ms" /></div>
        </Setting>
        <Setting label="Prompt" hint="What the describer is asked." fields={["boons.vision_describe_prompt"]}>
          <TextArea name="boons.vision_describe_prompt" label="Describe prompt" rows={3} defaultValue={b.vision_describe_prompt} className="font-sans text-[12.5px]" />
        </Setting>
      </>)}

      {row("structured_output", <>
        <Setting label="Fixer" hint="The model that repairs a reply that breaks the schema." fields={["boons.structured_output_fixer_model"]}>
          <div className="w-72"><SelectField name="boons.structured_output_fixer_model" label="Fixer model" value={picks.structured_output_fixer_model} onChange={pick("structured_output_fixer_model")} options={modelOptions(models, chat, "The same model, asked again", b.structured_output_fixer_model)} /></div>
        </Setting>
        <Setting label="Retries" hint="Up to 3, each within the timeout." fields={["boons.structured_output_max_repair_attempts", "boons.structured_output_timeout_ms"]}>
          <div className="flex flex-wrap gap-3"><NumberField name="structured_output_max_repair_attempts" label="Retries" value={b.structured_output_max_repair_attempts} unit="retries" className="w-20" /><NumberField name="structured_output_timeout_ms" label="Repair timeout" value={b.structured_output_timeout_ms} unit="ms" /></div>
        </Setting>
      </>)}

      {row("tool_loop", <>
        <Setting label="Limits" hint="Rounds of tool calls, time per tool, and time for the whole request." fields={["boons.tool_loop_max_turns", "boons.tool_loop_tool_timeout_ms", "boons.tool_loop_deadline_secs"]}>
          <div className="flex flex-wrap gap-3"><NumberField name="tool_loop_max_turns" label="Turns" value={b.tool_loop_max_turns} unit="turns" className="w-20" /><NumberField name="tool_loop_tool_timeout_ms" label="Tool timeout" value={b.tool_loop_tool_timeout_ms} unit="ms a tool" /><NumberField name="tool_loop_deadline_secs" label="Request deadline" value={b.tool_loop_deadline_secs} unit="s in all" /></div>
        </Setting>
        <Setting label="Nudge" hint="Added to the system prompt so the model knows it has tools. Blank puts back the default." fields={["boons.tool_loop_nudge"]}>
          <TextArea name="boons.tool_loop_nudge" label="Tool nudge" rows={3} defaultValue={b.tool_loop_nudge} className="font-sans text-[12.5px]" />
        </Setting>
      </>)}

      {row("image_generation", <>
        <Setting label="Image model" hint="The model that draws when a chat model asks." fields={["boons.image_generation_model"]} was={{ field: "boons.image_generation_model" }}>
          <div className="w-72"><SelectField name="boons.image_generation_model" label="Image model" value={picks.image_generation_model} onChange={pick("image_generation_model")} options={modelOptions(models, (m) => m.model_type === "image", "None", b.image_generation_model)} /></div>
        </Setting>
        <Setting label="Limits" hint="Images per call, how long one may take, and the sizes allowed (e.g. 512x512 1024x1024)." fields={["boons.image_generation_max_images_per_request", "boons.image_generation_timeout_ms", "boons.image_generation_allowed_sizes"]}>
          <div className="flex flex-wrap gap-3">
            <NumberField name="image_generation_max_images_per_request" label="Images per call" value={b.image_generation_max_images_per_request} unit="a call" className="w-20" />
            <NumberField name="image_generation_timeout_ms" label="Image timeout" value={b.image_generation_timeout_ms} unit="ms" />
            <TextField name="boons.image_generation_allowed_sizes" label="Allowed sizes" defaultValue={(b.image_generation_allowed_sizes ?? []).join(" ")} mono className="w-64" />
          </div>
        </Setting>
        <Setting label="Tool description" hint="What the chat model is told the drawing tool does." fields={["boons.image_generation_tool_description"]}>
          <TextArea name="boons.image_generation_tool_description" label="Tool description" rows={3} defaultValue={b.image_generation_tool_description} className="font-sans text-[12.5px]" />
        </Setting>
      </>)}

      {row("speculation", <>
        <input type="hidden" name="boons.speculation_gates" value={JSON.stringify(spec.gates)} />
        <Setting label="Profile" hint="How eagerly it drafts." fields={["boons.speculation_gates", "boons.speculation_unlisted", "boons.speculation_agree_min", "boons.speculation_lp_min"]}>
          <div className="grid gap-2 lg:grid-cols-3">
            {SPEC_PROFILES.map((p) => (
              <button key={p.key} type="button" aria-pressed={profile === p.key} onClick={() => setSpec(p.values)} className={cn("flex flex-col gap-0.5 rounded-lg border px-3 py-2 text-left", profile === p.key ? "border-foreground bg-secondary" : "border-border hover:border-muted-foreground/60")}>
                <span className="text-[12.5px] font-medium">{p.label}</span>
                <span className="text-[11.5px] leading-snug text-muted-foreground">{p.blurb}</span>
              </button>
            ))}
            <div className={cn("flex flex-col gap-0.5 rounded-lg border px-3 py-2", profile === "custom" ? "border-foreground bg-secondary" : "border-border")}>
              <span className="text-[12.5px] font-medium">Custom</span>
              <span className="text-[11.5px] leading-snug text-muted-foreground">Your own rules below.</span>
            </div>
          </div>
        </Setting>
        <Setting label="Drafter · classifier" hint="The fast model that drafts, and the one that sorts questions into categories." fields={["boons.speculation_draft_model", "boons.speculation_classify_model"]}>
          <div className="flex flex-wrap gap-2">
            <div className="w-60"><SelectField name="boons.speculation_draft_model" label="Drafter" value={picks.speculation_draft_model} onChange={pick("speculation_draft_model")} options={modelOptions(models, chat, "None", b.speculation_draft_model)} /></div>
            <div className="w-60"><SelectField name="boons.speculation_classify_model" label="Classifier" value={picks.speculation_classify_model} onChange={pick("speculation_classify_model")} options={modelOptions(models, chat, "None", b.speculation_classify_model)} /></div>
          </div>
        </Setting>
        <Setting label="Categories" hint="Where a draft is tried, and how sure the model must be to ship it. Blank uses the bar below." fields={["boons.speculation_gates", "boons.speculation_unlisted"]}>
          <GatesEditor gates={spec.gates} onChange={(gates) => setSpec((x) => ({ ...x, gates }))} />
          <Switch name="boons.speculation_unlisted" label="Draft for categories not listed" checked={spec.unlisted} onChange={(unlisted) => setSpec((x) => ({ ...x, unlisted }))}>Try a draft for categories not listed</Switch>
        </Setting>
        <Setting label="Advanced" hint="Where the model scores a draft, the bars to ship or give up, pacing, and extra template arguments for the drafter.">
          <button type="button" onClick={() => setSpecAdvanced((v) => !v)} className="self-start text-[12.5px] text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">{specAdvanced ? "Hide" : "Show"} 12 more settings</button>
        </Setting>
        <div className={cn(!specAdvanced && "hidden")}>
          <Setting label="Scoring endpoint" hint="URL template for the model's own scoring call; {upstream} is its upstream name. Endpoints that don't support prompt log-probabilities fail." fields={["boons.speculation_verify_url_template"]}>
            <TextField name="boons.speculation_verify_url_template" label="Scoring endpoint" defaultValue={b.speculation_verify_url_template ?? ""} mono />
          </Setting>
          <Setting label="Ship when" hint="Agreement (0 to 1) and confidence (log-probability, below 0) a draft needs." fields={["boons.speculation_agree_min", "boons.speculation_lp_min"]}>
            <div className="flex flex-wrap gap-3"><SpecNumber k="agree_min" label="Agreement to ship" spec={spec} setSpec={setSpec} /><SpecNumber k="lp_min" label="Confidence to ship" spec={spec} setSpec={setSpec} /></div>
          </Setting>
          <Setting label="Give up when" hint="Below these, the draft is dropped and the model answers itself." fields={["boons.speculation_abort_agree", "boons.speculation_abort_lp"]}>
            <div className="flex flex-wrap gap-3"><SpecNumber k="abort_agree" label="Agreement to give up" spec={spec} setSpec={setSpec} /><SpecNumber k="abort_lp" label="Confidence to give up" spec={spec} setSpec={setSpec} /></div>
          </Setting>
          <Setting label="Pacing" hint="Tokens checked first, then per check, the point it must decide by, and the most a draft may run." fields={["boons.speculation_first_chunk_tokens", "boons.speculation_chunk_tokens", "boons.speculation_decide_by_tokens", "boons.speculation_max_draft_tokens", "boons.speculation_pace_ms", "boons.speculation_timeout_ms"]}>
            <div className="flex flex-wrap gap-3">
              <SpecNumber k="first_chunk_tokens" label="First check" unit="tokens" className="w-20" spec={spec} setSpec={setSpec} />
              <SpecNumber k="chunk_tokens" label="Every" unit="tokens" className="w-20" spec={spec} setSpec={setSpec} />
              <SpecNumber k="decide_by_tokens" label="Decide by" unit="tokens" className="w-20" spec={spec} setSpec={setSpec} />
              <SpecNumber k="max_draft_tokens" label="Longest draft" unit="tokens" className="w-24" spec={spec} setSpec={setSpec} />
              <SpecNumber k="pace_ms" label="Streaming pace" unit="ms a token" className="w-16" spec={spec} setSpec={setSpec} />
              <SpecNumber k="timeout_ms" label="Budget" unit="ms in all" spec={spec} setSpec={setSpec} />
            </div>
          </Setting>
          <Setting label="Drafter template arguments" hint="JSON passed to the drafter's chat template, e.g. to turn off its thinking." fields={["boons.speculation_draft_chat_template_kwargs"]}>
            <TextArea name="boons.speculation_draft_chat_template_kwargs" label="Drafter template arguments" rows={2} defaultValue={Object.keys(b.speculation_draft_chat_template_kwargs ?? {}).length ? JSON.stringify(b.speculation_draft_chat_template_kwargs) : ""} placeholder='{"reasoning": false}' />
          </Setting>
        </div>
      </>)}

      {row("compression", <>
        <Setting label="What it does" hint="Lossless JSON compaction always; these add more. Lossy needs the compressor service." fields={["boons.compression_code_compaction", "boons.compression_dedup", "boons.compression_compact_logs", "boons.compression_allow_lossy"]}>
          <div className="grid gap-2 sm:grid-cols-2">
            <Switch name="boons.compression_code_compaction" label="Code compaction" defaultChecked={b.compression_code_compaction}>Strip spare whitespace from code</Switch>
            <Switch name="boons.compression_compact_logs" label="Log compaction" defaultChecked={b.compression_compact_logs}>Collapse repeated log lines</Switch>
            <Switch name="boons.compression_dedup" label="Cross-turn dedup" defaultChecked={b.compression_dedup}>Replace repeated blocks across turns</Switch>
            <Switch name="boons.compression_allow_lossy" label="Lossy" defaultChecked={b.compression_allow_lossy}>Summarize long prose{compressor?.configured ? "" : " (no compressor set up)"}</Switch>
          </div>
        </Setting>
        <Setting label="Limits" hint="Smallest input it touches, pieces per request, lossy pieces, how long originals are kept to retrieve, and how much prose lossy keeps." fields={["boons.compression_min_tokens", "boons.compression_max_segments", "boons.compression_max_lossy_segments", "boons.compression_original_ttl_secs", "boons.compression_neural_keep_ratio"]}>
          <div className="flex flex-wrap gap-3">
            <NumberField name="compression_min_tokens" label="Smallest input" value={b.compression_min_tokens} unit="tokens" className="w-20" />
            <NumberField name="compression_max_segments" label="Pieces" value={b.compression_max_segments} unit="pieces" className="w-20" />
            <NumberField name="compression_max_lossy_segments" label="Lossy pieces" value={b.compression_max_lossy_segments} unit="lossy" className="w-16" />
            <NumberField name="compression_original_ttl_secs" label="Originals kept" value={b.compression_original_ttl_secs} unit="s" />
            <NumberField name="compression_neural_keep_ratio" label="Prose kept" value={b.compression_neural_keep_ratio} unit="of prose" className="w-20" />
          </div>
        </Setting>
      </>)}

      <div id="boon-knowledge" className="grid min-h-[60px] grid-cols-[44px_minmax(0,1.1fr)_minmax(0,1.5fr)_minmax(0,1fr)_84px] items-center gap-3.5 border-t border-border px-[18px] py-2.5 text-[13px]">
        <span className="flex justify-center"><Glyph glyph={knowledge?.enabled ? "on" : "off"} /></span>
        <span className="flex flex-col"><span className="font-medium">Knowledge</span><span className="text-[11.5px] text-muted-foreground">Quotes your documents to the model</span></span>
        <span className="text-muted-foreground">Retrieval is {knowledge?.enabled ? "on" : "off"} · set on the Knowledge page</span>
        <span className="text-muted-foreground">{models.filter((m) => (m.boons ?? []).includes("knowledge")).length || "none"}</span>
        <Link href="/knowledge?tab=retrieval" className="inline-flex h-8 items-center justify-center rounded-lg border border-border px-3 text-[12.5px] hover:border-muted-foreground/60">Knowledge ›</Link>
      </div>
    </SettingsCard>
  );
}
