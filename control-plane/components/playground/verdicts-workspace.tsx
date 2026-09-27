"use client";

import { useState } from "react";
import { Copy, Loader2, Plus, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import type { ModelRoute } from "@/lib/obleth";
import { cn } from "@/lib/utils";
import type { PlaygroundSession } from "./playground";
import { Segmented } from "./ui";

type QuestionDraft = NonNullable<PlaygroundSession["verdictQuestions"]>[number];

interface VerdictResult {
  type: "boolean" | "choice" | "score";
  value: boolean | string | number;
  probabilities: Record<string, number> | number[];
  confidence: number;
  expected_value?: number;
}

interface RunResult {
  model: string;
  verdicts: Record<string, VerdictResult>;
  usage: { prompt_tokens: number; completion_tokens: number; total_tokens: number } | null;
  latencyMs: number;
  requestId: string | null;
}

const DEFAULT_QUESTION: QuestionDraft = {
  id: "is_urgent",
  type: "boolean",
  instructions: "Does the state describe an urgent problem?",
};

/** Turn the builder rows into the gateway's `questions` map. */
export function buildQuestions(drafts: QuestionDraft[]): Record<string, unknown> {
  const questions: Record<string, unknown> = {};
  drafts.forEach((d, i) => {
    const id = d.id.trim() || `q${i + 1}`;
    if (d.type === "boolean") {
      const criteria =
        d.trueDesc?.trim() || d.falseDesc?.trim()
          ? {
              ...(d.trueDesc?.trim() ? { true: d.trueDesc.trim() } : {}),
              ...(d.falseDesc?.trim() ? { false: d.falseDesc.trim() } : {}),
            }
          : undefined;
      questions[id] = { type: "boolean", instructions: d.instructions, ...(criteria ? { criteria } : {}) };
    } else if (d.type === "choice") {
      const criteria: Record<string, string | null> = {};
      for (const o of d.options ?? []) {
        if (o.name.trim()) criteria[o.name.trim()] = o.description.trim() || null;
      }
      questions[id] = { type: "choice", instructions: d.instructions, criteria };
    } else {
      questions[id] = {
        type: "score",
        instructions: d.instructions,
        criteria: (d.levels ?? []).map((l) => l.trim()).filter(Boolean),
      };
    }
  });
  return questions;
}

/**
 * The state textarea accepts either plain text or JSON. JSON is sent
 * structured so the gateway renders it the way a real client's structured
 * state would render — pasting `{"ticket": ...}` and getting it evaluated as
 * one string would be a misleading test.
 */
export function parseState(raw: string): unknown {
  const trimmed = raw.trim();
  if (trimmed.startsWith("{") || trimmed.startsWith("[")) {
    try {
      return JSON.parse(trimmed);
    } catch {
      /* fall through: send as text */
    }
  }
  return raw;
}

const TYPE_LABELS: Record<QuestionDraft["type"], string> = { boolean: "Yes / No", choice: "Choice", score: "Score" };

/** A fresh question of each type, with the scaffolding its builder needs. */
const newQuestion = (type: QuestionDraft["type"], n: number): QuestionDraft =>
  type === "choice"
    ? { id: `q${n}`, type, instructions: "", options: [{ name: "", description: "" }, { name: "", description: "" }] }
    : type === "score"
    ? { id: `q${n}`, type, instructions: "", levels: ["", "", "", "", ""] }
    : { id: `q${n}`, type, instructions: "" };

/** One probability row: label, bar, percentage. The chosen answer is the solid one. */
function ProbabilityBar({ label, p, chosen }: { label: string; p: number; chosen: boolean }) {
  return (
    <div className={cn("grid grid-cols-[6rem_minmax(0,1fr)_3rem] items-center gap-2.5 text-xs", chosen ? "font-medium text-foreground" : "text-muted-foreground")}>
      <span className="truncate" title={label}>{label}</span>
      <div className="h-1.5 overflow-hidden rounded-full bg-muted">
        <div className={cn("h-full rounded-full", chosen ? "bg-foreground" : "bg-muted-foreground/50")} style={{ width: `${Math.max(1, Math.round(p * 100))}%` }} />
      </div>
      <span className="text-right font-mono tabular-nums">{(p * 100).toFixed(1)}%</span>
    </div>
  );
}

/** Render one verdict as label/bar rows, whatever the question type. */
function VerdictCard({ id, verdict, draft }: { id: string; verdict: VerdictResult; draft?: QuestionDraft }) {
  let rows: { label: string; p: number; chosen: boolean }[] = [];
  let valueLabel = String(verdict.value);
  if (verdict.type === "boolean") {
    const probs = verdict.probabilities as Record<string, number>;
    rows = [
      { label: "yes", p: probs["true"] ?? 0, chosen: verdict.value === true },
      { label: "no", p: probs["false"] ?? 0, chosen: verdict.value === false },
    ];
    valueLabel = verdict.value ? "yes" : "no";
  } else if (verdict.type === "choice") {
    const probs = verdict.probabilities as Record<string, number>;
    rows = Object.entries(probs)
      .sort((a, b) => b[1] - a[1])
      .map(([option, p]) => ({ label: option, p, chosen: option === verdict.value }));
  } else {
    const probs = verdict.probabilities as number[];
    rows = probs.map((p, i) => ({
      label: draft?.levels?.[i] ? `${i + 1} · ${draft.levels[i]}` : `level ${i + 1}`,
      p,
      chosen: i + 1 === verdict.value,
    }));
    valueLabel = `${verdict.value} of ${probs.length}`;
  }
  return (
    <div className="space-y-2 border-t border-border pt-3.5 first:border-t-0 first:pt-0">
      <div className="flex items-baseline justify-between gap-2">
        <span className="truncate font-mono text-xs text-secondary-foreground">{id}</span>
        <span className="shrink-0 text-lg font-semibold">{valueLabel}</span>
      </div>
      <div className="flex justify-between text-[11.5px] text-muted-foreground">
        <span>confidence <span className="font-medium tabular-nums text-foreground">{(verdict.confidence * 100).toFixed(0)}%</span></span>
        {verdict.expected_value !== undefined && <span>expected {verdict.expected_value.toFixed(2)}</span>}
      </div>
      <div className="space-y-1.5">
        {rows.map((r) => <ProbabilityBar key={r.label} label={r.label} p={r.p} chosen={r.chosen} />)}
      </div>
    </div>
  );
}

const fieldCls = "w-full rounded-lg border border-border bg-background px-2.5 py-2 text-[13px] outline-none placeholder:text-muted-foreground focus:ring-1 focus:ring-ring";

function QuestionEditor({ draft, onChange, onRemove }: {
  draft: QuestionDraft;
  onChange: (next: QuestionDraft) => void;
  onRemove: () => void;
}) {
  const options = draft.options ?? [{ name: "", description: "" }, { name: "", description: "" }];
  const levels = draft.levels ?? ["", ""];
  return (
    <div className="space-y-2.5 rounded-xl border border-border bg-card/60 p-3">
      <div className="flex flex-wrap items-center gap-2">
        <Segmented
          label="Question type"
          value={draft.type}
          onChange={(type) => onChange({ ...draft, type })}
          className="w-auto"
          options={(["boolean", "choice", "score"] as const).map((t) => ({ value: t, label: TYPE_LABELS[t] }))}
        />
        <Input aria-label="Question id" className="h-8 min-w-0 flex-1 font-mono text-xs" maxLength={64} placeholder="question_id" value={draft.id}
          onChange={(e) => onChange({ ...draft, id: e.target.value })} />
        <Button variant="ghost" size="icon" className="h-8 w-8 shrink-0 text-muted-foreground" title="Remove question" aria-label="Remove question" onClick={onRemove}>
          <Trash2 className="h-3.5 w-3.5" />
        </Button>
      </div>
      <textarea aria-label="Question instructions" value={draft.instructions} maxLength={4000} rows={2}
        placeholder="What should be decided about the state?"
        className={cn(fieldCls, "resize-y")}
        onChange={(e) => onChange({ ...draft, instructions: e.target.value })} />
      {draft.type === "boolean" && (
        <div className="grid gap-2 sm:grid-cols-2">
          <Input aria-label="What yes means" className="h-8 text-xs" maxLength={1000} placeholder="What yes means (optional)" value={draft.trueDesc ?? ""}
            onChange={(e) => onChange({ ...draft, trueDesc: e.target.value })} />
          <Input aria-label="What no means" className="h-8 text-xs" maxLength={1000} placeholder="What no means (optional)" value={draft.falseDesc ?? ""}
            onChange={(e) => onChange({ ...draft, falseDesc: e.target.value })} />
        </div>
      )}
      {draft.type === "choice" && (
        <div className="space-y-1.5 rounded-lg bg-background/60 p-2">
          {options.map((o, i) => (
            <div key={i} className="flex gap-1.5">
              <Input aria-label={`Option ${i + 1} name`} className="h-8 w-32 font-mono text-xs" maxLength={200} placeholder="option" value={o.name}
                onChange={(e) => onChange({ ...draft, options: options.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)) })} />
              <Input aria-label={`Option ${i + 1} description`} className="h-8 min-w-0 flex-1 text-xs" maxLength={1000} placeholder="what this option means" value={o.description}
                onChange={(e) => onChange({ ...draft, options: options.map((x, j) => (j === i ? { ...x, description: e.target.value } : x)) })} />
              <Button variant="ghost" size="icon" className="h-8 w-8 shrink-0 text-muted-foreground" title="Remove option" aria-label={`Remove option ${i + 1}`}
                disabled={options.length <= 2}
                onClick={() => onChange({ ...draft, options: options.filter((_, j) => j !== i) })}>
                <Trash2 className="h-3 w-3" />
              </Button>
            </div>
          ))}
          <Button variant="ghost" size="sm" className="text-muted-foreground" disabled={options.length >= 26}
            onClick={() => onChange({ ...draft, options: [...options, { name: "", description: "" }] })}>
            <Plus className="h-3 w-3" />Option
          </Button>
        </div>
      )}
      {draft.type === "score" && (
        <div className="space-y-1.5 rounded-lg bg-background/60 p-2">
          {levels.map((l, i) => (
            <div key={i} className="flex items-center gap-1.5">
              <span className="w-6 text-right font-mono text-xs text-muted-foreground">{i + 1}</span>
              <Input aria-label={`Level ${i + 1} description`} className="h-8 min-w-0 flex-1 text-xs" maxLength={500} placeholder="what this level looks like" value={l}
                onChange={(e) => onChange({ ...draft, levels: levels.map((x, j) => (j === i ? e.target.value : x)) })} />
              <Button variant="ghost" size="icon" className="h-8 w-8 shrink-0 text-muted-foreground" title="Remove level" aria-label={`Remove level ${i + 1}`}
                disabled={levels.length <= 2}
                onClick={() => onChange({ ...draft, levels: levels.filter((_, j) => j !== i) })}>
                <Trash2 className="h-3 w-3" />
              </Button>
            </div>
          ))}
          <Button variant="ghost" size="sm" className="text-muted-foreground" disabled={levels.length >= 10}
            onClick={() => onChange({ ...draft, levels: [...levels, ""] })}>
            <Plus className="h-3 w-3" />Level
          </Button>
        </div>
      )}
    </div>
  );
}

function PaneHeader({ step, title, children }: { step: number; title: React.ReactNode; children?: React.ReactNode }) {
  return (
    <div className="flex h-12 shrink-0 items-center justify-between gap-2 border-b border-border pl-4 pr-3">
      <span className="flex items-center gap-2 text-[13px] font-semibold"><span className="text-[11px] font-medium text-muted-foreground">{step}</span>{title}</span>
      {children}
    </div>
  );
}

/**
 * The Verdicts mode of the Playground: paste a state (text or JSON), define
 * typed questions, and get typed verdicts with probability distributions and
 * confidence — the gateway's /v1/verdicts endpoint driven as the reserved
 * internal tenant via /api/live/playground/verdicts. Laid out left to right in
 * the order the work happens: state, questions, results.
 */
export function VerdictsWorkspace({ session, update, models, loading }: {
  session: PlaygroundSession;
  update: (patch: Partial<PlaygroundSession>) => void;
  models: ModelRoute[];
  loading: boolean;
}) {
  const chatModels = models.filter((m) => m.model_type === "chat");
  const model = session.verdictModel ?? "auto";
  const stateText = session.verdictState ?? "";
  const questions = session.verdictQuestions ?? [DEFAULT_QUESTION];

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<RunResult | null>(null);

  const canRun =
    !busy &&
    stateText.trim().length > 0 &&
    questions.length > 0 &&
    questions.every((q) => q.instructions.trim().length > 0);

  const run = async () => {
    if (!canRun) return;
    setBusy(true);
    setError(null);
    try {
      const res = await fetch("/api/live/playground/verdicts", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          model,
          state: parseState(stateText),
          questions: buildQuestions(questions),
        }),
      });
      const json = await res.json().catch(() => null);
      if (!res.ok) {
        throw new Error(json?.error || `Verdict request failed (HTTP ${res.status}).`);
      }
      setResult(json as RunResult);
    } catch (e) {
      setResult(null);
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const draftById = new Map(questions.map((q, i) => [q.id.trim() || `q${i + 1}`, q]));
  const trimmed = stateText.trim();
  const looksJson = trimmed.startsWith("{") || trimmed.startsWith("[");
  const stateKind = !trimmed ? "" : !looksJson ? "Plain text" : typeof parseState(stateText) === "string" ? "Not valid JSON, sent as text" : "Valid JSON";
  const addQuestion = (type: QuestionDraft["type"]) => update({ verdictQuestions: [...questions, newQuestion(type, questions.length + 1)] });

  return (
    <div
      className="flex h-full min-h-0 flex-col overflow-y-auto lg:flex-row lg:overflow-hidden"
      onKeyDown={(e) => { if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); void run(); } }}
    >
      <section aria-label="State" className="flex min-h-[20rem] flex-col border-b border-border lg:w-[340px] lg:shrink-0 lg:border-b-0 lg:border-r">
        <PaneHeader step={1} title="State to judge" />
        <div className="flex min-h-0 flex-1 flex-col gap-2 p-4">
          <textarea aria-label="State" value={stateText} maxLength={100_000}
            placeholder={'My card was charged twice for order A-104.\n\n…or paste JSON: {"ticket": {...}, "order": {...}}'}
            className={cn(fieldCls, "min-h-48 flex-1 resize-none p-3 font-mono text-xs leading-relaxed")}
            onChange={(e) => update({ verdictState: e.target.value })} />
          <div className="flex justify-between text-[11.5px] text-muted-foreground"><span>{stateKind}</span><span className="font-mono">{stateText.length.toLocaleString()} chars</span></div>
        </div>
      </section>

      <section aria-label="Questions" className="flex min-w-0 flex-col border-b border-border lg:flex-1 lg:border-b-0 lg:border-r">
        <PaneHeader step={2} title={<>Questions <span className="font-medium text-muted-foreground">{questions.length}</span></>} />
        <div className="min-h-0 flex-1 space-y-2.5 overflow-y-auto p-4">
          {questions.map((q, i) => (
            <QuestionEditor key={i} draft={q}
              onChange={(next) => update({ verdictQuestions: questions.map((x, j) => (j === i ? next : x)) })}
              onRemove={() => update({ verdictQuestions: questions.filter((_, j) => j !== i) })} />
          ))}
          <div className="flex flex-wrap items-center gap-1.5 pt-1">
            <span className="mr-0.5 text-xs text-muted-foreground">Add</span>
            {(["boolean", "choice", "score"] as const).map((t) => (
              <Button key={t} variant="outline" size="sm" disabled={questions.length >= 32} onClick={() => addQuestion(t)}>
                <Plus className="h-3 w-3" />{TYPE_LABELS[t]}
              </Button>
            ))}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-2 border-t border-border px-4 py-3">
          <Select aria-label="Verdict model" className="w-56"
            value={model}
            onValueChange={(value) => update({ verdictModel: value })}
            options={[
              { value: "auto", label: "auto (router picks)" },
              ...chatModels.map((m) => ({ value: m.model_name, label: m.model_name })),
            ]} />
          {loading && <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" aria-label="Loading models" />}
          <span className="ml-auto hidden text-xs text-muted-foreground sm:inline">Ctrl + Enter</span>
          <Button disabled={!canRun} onClick={() => void run()} aria-label="Run verdicts">
            {busy && <Loader2 className="h-4 w-4 animate-spin" />}
            Run verdicts
          </Button>
        </div>
      </section>

      <section aria-label="Results" className="flex min-h-[16rem] flex-col bg-card lg:w-[360px] lg:shrink-0">
        <PaneHeader step={3} title="Results">
          {result && (
            <Button variant="ghost" size="sm" className="h-7 text-xs text-muted-foreground" onClick={() => void navigator.clipboard.writeText(JSON.stringify(result.verdicts, null, 2)).catch(() => {})}>
              <Copy className="h-3.5 w-3.5" />Copy JSON
            </Button>
          )}
        </PaneHeader>
        <div className="min-h-0 flex-1 space-y-4 overflow-y-auto p-4">
          {error && <p role="alert" className="rounded-lg border border-border bg-secondary/50 p-3 text-sm">{error}</p>}
          {!result && !error && (
            <p className="rounded-xl border border-dashed border-border p-5 text-center text-[13px] leading-relaxed text-muted-foreground">
              {busy ? "Asking…" : <>Verdicts appear here: a typed answer per question, its probability distribution, and how decisively the model answered. Each question is one single-token call — no text generation, nothing to parse.</>}
            </p>
          )}
          {result && (
            <>
              <div className="flex flex-wrap gap-x-3 gap-y-1 font-mono text-[11.5px] text-muted-foreground">
                <span>{model === "auto" ? "routed to " : ""}<span className="text-foreground">{result.model}</span></span>
                <span>{result.latencyMs} ms</span>
                {result.usage && <span>{result.usage.total_tokens} tokens</span>}
                {result.requestId && <span title="Request id">{result.requestId}</span>}
              </div>
              <div className="space-y-3.5">
                {Object.entries(result.verdicts).map(([id, verdict]) => (
                  <VerdictCard key={id} id={id} verdict={verdict} draft={draftById.get(id)} />
                ))}
              </div>
            </>
          )}
        </div>
      </section>
    </div>
  );
}
