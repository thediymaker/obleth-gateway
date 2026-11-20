"use client";

import { memo, useState, useTransition } from "react";
import { setModelReasoningAction } from "@/app/actions";
import { SelectField, Setting, TextArea } from "@/components/models/fields";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  LEVEL_PRESETS,
  levelAction,
  REASONING_LEVEL_LABELS,
  REASONING_LEVELS,
  reasoningDirty,
  reasoningDraft,
  reasoningFromDraft,
  reasoningPreview,
  type LevelDraft,
  type LevelPreset,
  type ReasoningDraft,
} from "@/lib/models-model";
import type { ModelReasoning, ModelRoute, ReasoningAction, ReasoningLevel, ReasoningMode } from "@/lib/obleth";

const MODES: { value: ReasoningMode; label: string; hint: string }[] = [
  {
    value: "passthrough",
    label: "Pass through — send what callers send",
    hint: "Requests reach the model exactly as callers wrote them, effort included. Right for a server that reads reasoning_effort the way callers use it.",
  },
  {
    value: "mapped",
    label: "Translate the four levels",
    hint: "obleth takes the caller's effort out of the request and puts in the fields you choose for that level below.",
  },
  {
    value: "unsupported",
    label: "This model has no effort control — remove it from requests",
    hint: "obleth takes reasoning_effort and reasoning out of every request, so a server that would reject them never sees them.",
  },
];

const HEADER_HINTS: Record<ReasoningMode, string> = {
  passthrough: "Nothing is added to the response for this model.",
  mapped: "Each response says which level was served in x-obleth-reasoning-effort. When a level was served as another, x-obleth-reasoning-effort-requested says what the caller asked for.",
  unsupported: "Each response carries x-obleth-reasoning-effort: unsupported, so callers can tell their effort was dropped.",
};

/** The map the preview describes: the rows that can be built, whatever else is unfinished. */
function previewMap(draft: ReasoningDraft): ModelReasoning {
  const levels: Partial<Record<ReasoningLevel, ReasoningAction>> = {};
  for (const level of REASONING_LEVELS) {
    const built = levelAction(draft.levels[level]);
    if (built.ok) levels[level] = built.action;
  }
  return { mode: draft.mode, default_level: draft.defaultLevel || null, levels };
}

/**
 * How the model takes reasoning effort. Callers ask for none, low, medium or
 * high; servers disagree about what those mean, so each level can become
 * whatever fields this model's server understands. Saved on its own, at
 * once, like the lifecycle: the map has its own write.
 */
export const ReasoningSection = memo(function ReasoningSection({ model, onChanged }: { model: ModelRoute; onChanged: () => void }) {
  const [draft, setDraft] = useState<ReasoningDraft>(() => reasoningDraft(model));
  const [error, setError] = useState<string | null>(null);
  const [busy, start] = useTransition();

  const dirty = reasoningDirty(draft, model);
  const preview = reasoningPreview(previewMap(draft));

  const setLevel = (level: ReasoningLevel, patch: Partial<LevelDraft>) =>
    setDraft((d) => ({ ...d, levels: { ...d.levels, [level]: { ...d.levels[level], ...patch } } }));

  function setPreset(level: ReasoningLevel, preset: LevelPreset) {
    const row = draft.levels[level];
    // Opening Custom on a row that sent something starts from those fields.
    const built = levelAction(row);
    const prefill = preset === "custom" && row.json.trim() === "{}" && built.ok && "send" in built.action ? JSON.stringify(built.action.send, null, 2) : row.json;
    setLevel(level, { preset, json: prefill });
  }

  function reset() {
    setDraft(reasoningDraft(model));
    setError(null);
  }

  function save() {
    const built = reasoningFromDraft(draft, model);
    if (!built.ok) {
      setError(built.error);
      return;
    }
    start(async () => {
      const result = await setModelReasoningAction(model.id, built.reasoning);
      if (!result.ok) {
        setError(result.error);
        return;
      }
      setError(null);
      onChanged();
    });
  }

  return (
    <section id="reasoning" data-section="reasoning" aria-label="Reasoning" className="scroll-mt-24 rounded-xl border border-border bg-card">
      <header className="flex flex-wrap items-start justify-between gap-3 px-[18px] pb-3 pt-4">
        <div>
          <h2 className="text-sm font-semibold">Reasoning</h2>
          <p className="mt-0.5 text-xs text-muted-foreground">
            Callers ask for one of four effort levels: none, low, medium or high. Model servers take effort in different ways, so choose what each level becomes for this one. Saved on its own, and applies within a few seconds.
          </p>
        </div>
        <div className="flex items-center gap-2">
          {dirty && <Button type="button" size="sm" variant="ghost" onClick={reset} disabled={busy}>Discard</Button>}
          <Button type="button" size="sm" onClick={save} disabled={!dirty || busy}>{busy ? "Saving…" : "Save reasoning"}</Button>
        </div>
      </header>
      {error && <p role="alert" className="mx-[18px] mb-3 rounded-lg border border-foreground/60 px-3 py-2 text-[12.5px]">{error}</p>}

      <Setting label="Effort on requests" hint="What obleth does with the effort a caller asks for before the request reaches this model.">
        <div role="radiogroup" aria-label="How the model takes effort" className="flex flex-col gap-1.5">
          {MODES.map((m) => (
            <label key={m.value} className="cursor-pointer">
              <input type="radio" name="reasoning_mode" value={m.value} checked={draft.mode === m.value} onChange={() => setDraft((d) => ({ ...d, mode: m.value }))} className="peer sr-only" />
              <span className="flex flex-col gap-0.5 rounded-lg border border-border px-3 py-2 transition-colors hover:border-muted-foreground/60 peer-checked:border-foreground/70 peer-checked:bg-secondary peer-focus-visible:ring-1 peer-focus-visible:ring-ring">
                <span className="text-[13px]">{m.label}</span>
                <span className="text-xs text-muted-foreground">{m.hint}</span>
              </span>
            </label>
          ))}
        </div>
      </Setting>

      {draft.mode === "mapped" && (
        <>
          <Setting
            label="What each level sends"
            hint="One row per level a caller can ask for. The fields you choose are added to the request. Where the caller already sent the same object, such as chat_template_kwargs, their other keys stay and your values win."
          >
            <div className="flex flex-col divide-y divide-border rounded-lg border border-border">
              {REASONING_LEVELS.map((level) => (
                <LevelRow
                  key={level}
                  level={level}
                  row={draft.levels[level]}
                  others={REASONING_LEVELS.filter((l) => l !== level && draft.levels[l].preset !== "use")}
                  onPreset={(preset) => setPreset(level, preset)}
                  onChange={(patch) => setLevel(level, patch)}
                />
              ))}
            </div>
          </Setting>
          <Setting
            label="When a caller sets no effort"
            hint="A request that names no level can go as it came, leaving the choice to the model's server, or be served as one of the four levels."
          >
            <div className="w-72">
              <SelectField
                label="When a caller sets no effort"
                value={draft.defaultLevel}
                onChange={(v) => setDraft((d) => ({ ...d, defaultLevel: v as ReasoningLevel | "" }))}
                options={[{ value: "", label: "Leave it to the model's server" }, ...REASONING_LEVELS.map((l) => ({ value: l, label: `Serve it as ${l}` }))]}
              />
            </div>
          </Setting>
        </>
      )}

      <Setting label="What requests become" hint={HEADER_HINTS[draft.mode]}>
        {preview.length === 1 ? (
          <p className="text-[12.5px] text-secondary-foreground">{preview[0]}</p>
        ) : (
          <ul aria-label="What each level becomes" className="flex flex-col gap-1 font-mono text-[12px] text-secondary-foreground">
            {preview.map((line) => <li key={line}>{line}</li>)}
          </ul>
        )}
      </Setting>
    </section>
  );
});

function LevelRow({ level, row, others, onPreset, onChange }: {
  level: ReasoningLevel;
  row: LevelDraft;
  /** Levels this one may be the same as: the other levels that send something. */
  others: ReasoningLevel[];
  onPreset: (preset: LevelPreset) => void;
  onChange: (patch: Partial<LevelDraft>) => void;
}) {
  const label = REASONING_LEVEL_LABELS[level];
  const built = levelAction(row);
  return (
    <div className="flex flex-col gap-2 px-3 py-2.5">
      <div className="flex flex-wrap items-center gap-2">
        <span className="w-16 shrink-0 text-[13px] font-medium">{label}</span>
        <div className="w-80 max-w-full">
          <SelectField label={`${label} becomes`} value={row.preset} onChange={(v) => onPreset(v as LevelPreset)} options={LEVEL_PRESETS} />
        </div>
        {row.preset === "effort" && (
          <Input
            aria-label={`reasoning_effort value for ${level}`}
            value={row.effort}
            onChange={(e) => onChange({ effort: e.target.value })}
            placeholder="high"
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
            className="h-9 w-36 font-mono text-[12.5px]"
          />
        )}
        {row.preset === "use" && (
          <div className="w-36">
            <SelectField label={`Level ${level} is served as`} value={others.includes(row.use) ? row.use : ""} onChange={(v) => onChange({ use: v as ReasoningLevel })} options={others.map((l) => ({ value: l, label: REASONING_LEVEL_LABELS[l] }))} />
          </div>
        )}
      </div>
      {row.preset === "custom" && (
        <TextArea name={`reasoning_custom_${level}`} label={`Custom fields for ${level}`} rows={3} value={row.json} onChange={(json) => onChange({ json })} placeholder='{"reasoning_effort": "high"}' />
      )}
      {row.preset === "use" && others.length === 0 && <p className="text-[12px] text-muted-foreground">Set another level to send something first, then this one can match it.</p>}
      {!built.ok && <p className="text-[12px] text-foreground">{built.error}</p>}
    </div>
  );
}
