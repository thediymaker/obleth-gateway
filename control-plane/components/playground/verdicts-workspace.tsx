"use client";

import { useState } from "react";
import { Loader2, Plus, Scale, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import type { ModelRoute } from "@/lib/obleth";
import { cn } from "@/lib/utils";
import type { PlaygroundSession } from "./playground";

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
function buildQuestions(drafts: QuestionDraft[]): Record<string, unknown> {
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
function parseState(raw: string): unknown {
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

/** One probability row: label, bar, percentage. */
function ProbabilityBar({ label, p, chosen }: { label: string; p: number; chosen: boolean }) {
  return (
    <div className="flex items-center gap-2 text-xs">
      <span className={cn("w-32 truncate text-right", chosen ? "font-semibold text-foreground" : "text-muted-foreground")} title={label}>
        {label}
      </span>
      <div className="h-2 flex-1 overflow-hidden rounded-full bg-muted">
        <div
          className={cn("h-full rounded-full", chosen ? "bg-foreground" : "bg-muted-foreground/50")}
          style={{ width: `${Math.max(1, Math.round(p * 100))}%` }}
        />
      </div>
      <span className="w-12 tabular-nums text-muted-foreground">{(p * 100).toFixed(1)}%</span>
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
    valueLabel = `level ${verdict.value}`;
  }
  return (
    <div className="space-y-2 rounded-lg border border-border p-3">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-xs font-semibold">{id}</span>
        <span className="rounded bg-secondary px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground">{verdict.type}</span>
        <span className="ml-auto text-xs text-muted-foreground">
          confidence <span className="font-medium tabular-nums text-foreground">{(verdict.confidence * 100).toFixed(0)}%</span>
        </span>
      </div>
      <p className="text-sm">
        <span className="rounded-md bg-secondary px-2 py-0.5 font-medium">{valueLabel}</span>
        {verdict.expected_value !== undefined && (
          <span className="ml-2 text-xs text-muted-foreground">expected {verdict.expected_value.toFixed(2)}</span>
        )}
      </p>
      <div className="space-y-1">
        {rows.map((r) => (
          <ProbabilityBar key={r.label} label={r.label} p={r.p} chosen={r.chosen} />
        ))}
      </div>
    </div>
  );
}

function QuestionEditor({ draft, onChange, onRemove }: {
  draft: QuestionDraft;
  onChange: (next: QuestionDraft) => void;
  onRemove: () => void;
}) {
  const options = draft.options ?? [{ name: "", description: "" }, { name: "", description: "" }];
  const levels = draft.levels ?? ["", ""];
  return (
    <div className="space-y-2 rounded-lg border border-border p-3">
      <div className="flex flex-wrap items-center gap-2">
        <Input aria-label="Question id" className="h-8 w-36 font-mono text-xs" maxLength={64} placeholder="question_id" value={draft.id}
          onChange={(e) => onChange({ ...draft, id: e.target.value })} />
        <Select aria-label="Question type" className="h-8 w-28"
          value={draft.type}
          onValueChange={(value) => onChange({ ...draft, type: value as QuestionDraft["type"] })}
          options={[
            { value: "boolean", label: "Boolean" },
            { value: "choice", label: "Choice" },
            { value: "score", label: "Score" },
          ]} />
        <Button variant="ghost" size="icon" className="ml-auto h-7 w-7 text-muted-foreground hover:text-destructive" title="Remove question" aria-label="Remove question" onClick={onRemove}>
          <Trash2 className="h-3.5 w-3.5" />
        </Button>
      </div>
      <textarea aria-label="Question instructions" value={draft.instructions} maxLength={4000} rows={2}
        placeholder="What should be decided about the state?"
        className="w-full resize-y rounded-md border border-border bg-background p-2 text-sm"
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
        <div className="space-y-1.5">
          {options.map((o, i) => (
            <div key={i} className="flex gap-2">
              <Input aria-label={`Option ${i + 1} name`} className="h-8 w-40 text-xs" maxLength={200} placeholder="option" value={o.name}
                onChange={(e) => onChange({ ...draft, options: options.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)) })} />
              <Input aria-label={`Option ${i + 1} description`} className="h-8 flex-1 text-xs" maxLength={1000} placeholder="what this option means" value={o.description}
                onChange={(e) => onChange({ ...draft, options: options.map((x, j) => (j === i ? { ...x, description: e.target.value } : x)) })} />
              <Button variant="ghost" size="icon" className="h-8 w-8 shrink-0 text-muted-foreground" title="Remove option" aria-label={`Remove option ${i + 1}`}
                disabled={options.length <= 2}
                onClick={() => onChange({ ...draft, options: options.filter((_, j) => j !== i) })}>
                <Trash2 className="h-3 w-3" />
              </Button>
            </div>
          ))}
          <Button variant="outline" size="sm" disabled={options.length >= 26}
            onClick={() => onChange({ ...draft, options: [...options, { name: "", description: "" }] })}>
            <Plus className="mr-1 h-3 w-3" />Option
          </Button>
        </div>
      )}
      {draft.type === "score" && (
        <div className="space-y-1.5">
          {levels.map((l, i) => (
            <div key={i} className="flex items-center gap-2">
              <span className="w-14 text-right text-xs text-muted-foreground">level {i + 1}</span>
              <Input aria-label={`Level ${i + 1} description`} className="h-8 flex-1 text-xs" maxLength={500} placeholder="what this level looks like" value={l}
                onChange={(e) => onChange({ ...draft, levels: levels.map((x, j) => (j === i ? e.target.value : x)) })} />
              <Button variant="ghost" size="icon" className="h-8 w-8 shrink-0 text-muted-foreground" title="Remove level" aria-label={`Remove level ${i + 1}`}
                disabled={levels.length <= 2}
                onClick={() => onChange({ ...draft, levels: levels.filter((_, j) => j !== i) })}>
                <Trash2 className="h-3 w-3" />
              </Button>
            </div>
          ))}
          <Button variant="outline" size="sm" disabled={levels.length >= 10}
            onClick={() => onChange({ ...draft, levels: [...levels, ""] })}>
            <Plus className="mr-1 h-3 w-3" />Level
          </Button>
        </div>
      )}
    </div>
  );
}

/**
 * The Verdicts mode of the Playground: paste a state (text or JSON), define
 * typed questions, and get typed verdicts with probability distributions and
 * confidence — the gateway's /v1/verdicts endpoint driven as the reserved
 * internal tenant via /api/live/playground/verdicts.
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

  return (
    <div className="grid h-full min-h-0 gap-4 overflow-y-auto p-4 lg:grid-cols-2">
      <div className="space-y-3">
        <label className="block text-xs font-medium">
          State — the content the questions are asked about (text or JSON)
          <textarea aria-label="State" value={stateText} maxLength={100_000} rows={8}
            placeholder={'My card was charged twice for order A-104.\n…or paste JSON: {"ticket": {...}, "order": {...}}'}
            className="mt-1 w-full resize-y rounded-md border border-border bg-background p-2 font-mono text-sm"
            onChange={(e) => update({ verdictState: e.target.value })} />
        </label>
        <div className="flex items-center gap-2">
          <Select aria-label="Verdict model" className="w-56"
            value={model}
            onValueChange={(value) => update({ verdictModel: value })}
            options={[
              { value: "auto", label: "auto (router picks)" },
              ...chatModels.map((m) => ({ value: m.model_name, label: m.model_name })),
            ]} />
          {loading && <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" aria-label="Loading models" />}
          <Button className="ml-auto" disabled={!canRun} onClick={run} aria-label="Run verdicts">
            {busy ? <Loader2 className="mr-2 h-4 w-4 animate-spin" /> : <Scale className="mr-2 h-4 w-4" />}
            Run
          </Button>
        </div>
        <div className="space-y-2">
          {questions.map((q, i) => (
            <QuestionEditor key={i} draft={q}
              onChange={(next) => update({ verdictQuestions: questions.map((x, j) => (j === i ? next : x)) })}
              onRemove={() => update({ verdictQuestions: questions.filter((_, j) => j !== i) })} />
          ))}
          <Button variant="outline" size="sm" disabled={questions.length >= 32}
            onClick={() => update({ verdictQuestions: [...questions, { id: `q${questions.length + 1}`, type: "boolean", instructions: "" }] })}>
            <Plus className="mr-1.5 h-3.5 w-3.5" />Add question
          </Button>
        </div>
      </div>
      <div className="space-y-3">
        {error && <p role="alert" className="rounded-md border border-destructive/40 bg-destructive/10 p-3 text-sm text-destructive">{error}</p>}
        {!result && !error && (
          <p className="rounded-lg border border-dashed border-border p-6 text-center text-sm text-muted-foreground">
            Verdicts appear here: a typed answer per question, its probability distribution, and how
            decisively the model answered. Each question is one single-token call against the
            model&apos;s own backend — no text generation, nothing to parse.
          </p>
        )}
        {result && (
          <>
            <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted-foreground">
              <span>model <span className="font-medium text-foreground">{result.model}</span></span>
              <span>{result.latencyMs} ms</span>
              {result.usage && <span>{result.usage.total_tokens} tokens ({result.usage.completion_tokens} generated)</span>}
              {result.requestId && <span className="font-mono">{result.requestId}</span>}
            </div>
            {Object.entries(result.verdicts).map(([id, verdict]) => (
              <VerdictCard key={id} id={id} verdict={verdict} draft={draftById.get(id)} />
            ))}
          </>
        )}
      </div>
    </div>
  );
}
