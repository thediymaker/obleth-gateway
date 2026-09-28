"use client";

import { useState } from "react";
import { Setting, Switch } from "@/components/models/fields";
import { Segmented } from "@/components/overview/ui";
import { Select } from "@/components/ui/select";
import type { CompressionPolicy, GuardrailsAction, GuardrailsPolicy } from "@/lib/obleth";
import { cn } from "@/lib/utils";

const ACTIONS: { value: GuardrailsAction; label: string; hint: string }[] = [
  { value: "block", label: "Block", hint: "Reject flagged requests and responses with an error." },
  { value: "redact", label: "Redact", hint: "Replace matched personal info and keywords in place, and carry on." },
  { value: "log_only", label: "Log only", hint: "Record an alert; never block or change traffic." },
];

const SCANNERS: Record<string, { label: string; hint: string }> = {
  pii: { label: "Personal info", hint: "SSNs, emails, phone and card numbers." },
  prompt_injection: { label: "Prompt injection", hint: "Jailbreak and instruction-override attempts. Always blocks, whatever the action." },
  ban_keywords: { label: "Banned keywords", hint: "The keyword list below: whole words, any case." },
  harm: { label: "Harmful content", hint: "Classified by a guard model, which you pick below." },
};

const INPUT_SCANNERS = ["pii", "prompt_injection", "ban_keywords", "harm"];
const OUTPUT_SCANNERS = ["pii", "ban_keywords", "harm"];

const PRESETS: { id: string; label: string; blurb: string; policy: GuardrailsPolicy }[] = [
  {
    id: "ferpa_pii",
    label: "FERPA / personal info",
    blurb: "Redacts personal info both ways. Passes through if a scanner fails.",
    policy: { action: "redact", input_scanners: ["pii"], output_scanners: ["pii"], guard_model: null, ban_keywords: [], fail_open: true },
  },
  {
    id: "injection_defense",
    label: "Prompt-injection defense",
    blurb: "Blocks injection attempts before they reach the model.",
    policy: { action: "block", input_scanners: ["prompt_injection"], output_scanners: [], guard_model: null, ban_keywords: [], fail_open: true },
  },
  {
    id: "monitor_only",
    label: "Monitor only",
    blurb: "Logs an alert when personal info appears. Never blocks.",
    policy: { action: "log_only", input_scanners: ["pii"], output_scanners: ["pii"], guard_model: null, ban_keywords: [], fail_open: true },
  },
];

function sameSet(a: string[], b: string[]) {
  return a.length === b.length && a.every((x) => b.includes(x));
}

export function policiesEqual(a: GuardrailsPolicy, b: GuardrailsPolicy): boolean {
  return (
    a.action === b.action &&
    sameSet(a.input_scanners, b.input_scanners) &&
    sameSet(a.output_scanners, b.output_scanners) &&
    (a.guard_model || null) === (b.guard_model || null) &&
    sameSet(a.ban_keywords, b.ban_keywords) &&
    a.fail_open === b.fail_open
  );
}

/** What a guardrails policy does, in a line. */
export function guardrailsSummary(p: GuardrailsPolicy | null): string {
  if (!p) return "Off: requests and responses pass unscreened.";
  const preset = PRESETS.find((x) => policiesEqual(x.policy, p));
  if (p.input_scanners.length === 0 && p.output_scanners.length === 0) return "On, but no scanner is chosen, so it does nothing.";
  const where = p.input_scanners.length && p.output_scanners.length ? "requests and responses" : p.input_scanners.length ? "requests" : "responses";
  const verb = p.action === "block" ? "Blocks flagged" : p.action === "redact" ? "Redacts matches in" : "Watches";
  return `${preset ? `${preset.label}. ` : ""}${verb} ${where}. ${p.fail_open ? "Passes through if a scanner fails." : "Refuses with 503 if a scanner fails."}`;
}

/** Whether a policy can be saved: the harmful-content scanner needs a guard model. */
export function guardrailsProblem(json: string): string | null {
  if (!json) return null;
  try {
    const p = JSON.parse(json) as GuardrailsPolicy;
    const harm = p.input_scanners.includes("harm") || p.output_scanners.includes("harm");
    return harm && !p.guard_model ? "Guardrails: pick a guard model for the harmful-content scanner." : null;
  } catch {
    return null;
  }
}

function Chip({ on, onClick, children, title }: { on: boolean; onClick: () => void; children: React.ReactNode; title?: string }) {
  return (
    <button
      type="button"
      aria-pressed={on}
      title={title}
      onClick={onClick}
      className={cn("inline-flex h-7 items-center rounded-full border px-2.5 text-[12px] transition-colors", on ? "border-muted-foreground bg-secondary text-foreground" : "border-border text-muted-foreground hover:text-foreground")}
    >
      {children}
    </button>
  );
}

/**
 * The guardrails policy as settings rows. It submits `guardrails_policy` as
 * JSON, blank when off.
 */
export function GuardrailsFields({ policy, models }: { policy: GuardrailsPolicy | null; models: string[] }) {
  const [on, setOn] = useState(policy != null);
  const [p, setP] = useState<GuardrailsPolicy>(policy ?? PRESETS[0].policy);
  const [keywords, setKeywords] = useState((policy?.ban_keywords ?? []).join("\n"));
  const current: GuardrailsPolicy = { ...p, ban_keywords: keywords.split("\n").map((k) => k.trim()).filter(Boolean) };
  const preset = PRESETS.find((x) => policiesEqual(x.policy, current))?.id ?? "custom";
  const [custom, setCustom] = useState(policy != null && preset === "custom");
  const toggle = (list: "input_scanners" | "output_scanners", name: string) =>
    setP((x) => ({ ...x, [list]: x[list].includes(name) ? x[list].filter((s) => s !== name) : [...x[list], name] }));
  const harm = current.input_scanners.includes("harm") || current.output_scanners.includes("harm");
  const usesKeywords = current.input_scanners.includes("ban_keywords") || current.output_scanners.includes("ban_keywords");
  const fields = ["guardrails_policy"];

  return (
    <>
      <input type="hidden" name="guardrails_policy" value={on ? JSON.stringify(current) : ""} />
      <Setting id="set-guardrails" label="Screen its traffic" hint={guardrailsSummary(on ? current : null)} fields={fields}>
        <Switch label="Screen its traffic" checked={on} onChange={setOn} />
      </Setting>
      {on && (
        <>
          <Setting id="set-guardrails-profile" label="Profile" hint="Start from one, then change anything below." fields={fields}>
            <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-4">
              {PRESETS.map((x) => (
                <button
                  key={x.id}
                  type="button"
                  aria-pressed={preset === x.id && !custom}
                  onClick={() => { setP(x.policy); setKeywords(""); setCustom(false); }}
                  className={cn("flex flex-col gap-0.5 rounded-lg border px-3 py-2 text-left", preset === x.id && !custom ? "border-foreground bg-secondary" : "border-border hover:border-muted-foreground/60")}
                >
                  <span className="text-[12.5px] font-medium">{x.label}</span>
                  <span className="text-[11.5px] leading-snug text-muted-foreground">{x.blurb}</span>
                </button>
              ))}
              <button
                type="button"
                aria-pressed={preset === "custom" || custom}
                onClick={() => setCustom(true)}
                className={cn("flex flex-col gap-0.5 rounded-lg border px-3 py-2 text-left", preset === "custom" || custom ? "border-foreground bg-secondary" : "border-border hover:border-muted-foreground/60")}
              >
                <span className="text-[12.5px] font-medium">Custom</span>
                <span className="text-[11.5px] leading-snug text-muted-foreground">Pick the action, scanners and keywords yourself.</span>
              </button>
            </div>
          </Setting>
          {(custom || preset === "custom") && (
            <>
              <Setting label="Action" hint={ACTIONS.find((a) => a.value === current.action)?.hint} fields={fields}>
                <Segmented label="Action" value={current.action} onChange={(action) => setP((x) => ({ ...x, action }))} options={ACTIONS} />
              </Setting>
              <Setting label="Scan requests" hint="Before they reach the model." fields={fields}>
                <div className="flex flex-wrap gap-1.5">
                  {INPUT_SCANNERS.map((s) => <Chip key={s} on={current.input_scanners.includes(s)} onClick={() => toggle("input_scanners", s)} title={SCANNERS[s].hint}>{SCANNERS[s].label}</Chip>)}
                </div>
              </Setting>
              <Setting label="Scan responses" hint="Blocking or redacting a response waits for the whole reply." fields={fields}>
                <div className="flex flex-wrap gap-1.5">
                  {OUTPUT_SCANNERS.map((s) => <Chip key={s} on={current.output_scanners.includes(s)} onClick={() => toggle("output_scanners", s)} title={SCANNERS[s].hint}>{SCANNERS[s].label}</Chip>)}
                </div>
              </Setting>
              {usesKeywords && (
                <Setting label="Banned keywords" hint="One per line. Whole words, any case." fields={fields}>
                  <textarea
                    aria-label="Banned keywords"
                    rows={3}
                    value={keywords}
                    onChange={(e) => setKeywords(e.target.value)}
                    placeholder={"confidential\nrestricted"}
                    className="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-[12.5px] focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                  />
                </Setting>
              )}
              {harm && (
                <Setting label="Guard model" hint={current.guard_model ? "Classifies harmful content." : "Required for the harmful-content scanner."} fields={fields}>
                  <div className="max-w-sm">
                    <Select
                      aria-label="Guard model"
                      value={current.guard_model ?? ""}
                      invalid={!current.guard_model}
                      onValueChange={(v) => setP((x) => ({ ...x, guard_model: v || null }))}
                      searchPlaceholder="Find a model"
                      className="h-9 text-[13px]"
                      options={[{ value: "", label: "Pick a model" }, ...models.map((m) => ({ value: m, label: m }))]}
                    />
                  </div>
                </Setting>
              )}
              <Setting label="If a scanner fails" hint={current.fail_open ? "The request goes through unchanged." : "The request is refused with 503."} fields={fields}>
                <Segmented
                  label="If a scanner fails"
                  value={current.fail_open ? "open" : "closed"}
                  onChange={(v) => setP((x) => ({ ...x, fail_open: v === "open" }))}
                  options={[{ value: "open", label: "Let it through" }, { value: "closed", label: "Refuse it" }]}
                />
              </Setting>
            </>
          )}
        </>
      )}
    </>
  );
}

const COMPRESSION_PARTS: { key: keyof Omit<CompressionPolicy, "enabled">; label: string; hint: string }[] = [
  { key: "code_compaction", label: "Code compaction", hint: "Strip spare whitespace from fenced code." },
  { key: "dedup", label: "Cross-turn dedup", hint: "Replace repeated blocks with a reference the model can expand." },
  { key: "compact_logs", label: "Log compaction", hint: "Collapse repeated log lines; errors and warnings stay." },
  { key: "allow_lossy", label: "Allow lossy", hint: "Summarize long prose with a helper model; the original stays retrievable." },
];

const OFF: CompressionPolicy = { enabled: false, code_compaction: false, dedup: false, compact_logs: false, allow_lossy: false };

export function compressionSummary(p: CompressionPolicy | null): string {
  if (!p) return "Follows the gateway's default (lossless JSON only).";
  if (!p.enabled) return "Off for this tenant.";
  const on = COMPRESSION_PARTS.filter((x) => p[x.key]).map((x) => x.label.toLowerCase());
  return on.length ? `On: ${on.join(", ")}.` : "On, lossless JSON only.";
}

/** The compression policy as settings rows; it submits `compression_policy` as JSON, blank to follow the default. */
export function CompressionFields({ policy }: { policy: CompressionPolicy | null }) {
  const [own, setOwn] = useState(policy != null);
  const [p, setP] = useState<CompressionPolicy>(policy ?? { ...OFF, enabled: true });
  const fields = ["compression_policy"];
  return (
    <>
      <input type="hidden" name="compression_policy" value={own ? JSON.stringify(p) : ""} />
      <Setting id="set-compression" label="Its own settings" hint={compressionSummary(own ? p : null)} fields={fields}>
        <Switch label="Its own compression settings" checked={own} onChange={setOwn} />
      </Setting>
      {own && (
        <>
          <Setting label="Compression" hint="The switch for everything below." fields={fields}>
            <Switch label="Compression on" checked={p.enabled} onChange={(enabled) => setP((x) => ({ ...x, enabled }))} />
          </Setting>
          {p.enabled && COMPRESSION_PARTS.map((part) => (
            <Setting key={part.key} label={part.label} hint={part.hint} fields={fields}>
              <Switch label={part.label} checked={p[part.key]} onChange={(v) => setP((x) => ({ ...x, [part.key]: v }))} />
            </Setting>
          ))}
          {p.enabled && (p.dedup || p.allow_lossy) && (
            <p className="border-t border-border px-[18px] py-2.5 text-[11.5px] text-muted-foreground">Dedup and lossy need a model with function calling and the gateway tool loop on.</p>
          )}
        </>
      )}
    </>
  );
}
