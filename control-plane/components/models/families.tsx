"use client";

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { Plus } from "lucide-react";
import { deleteModelFamilyAction, saveModelFamilyAction } from "@/app/actions";
import { Field, SelectField } from "@/components/models/fields";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import {
  familyDraft,
  familyGroups,
  familyMemberOptions,
  familyPreview,
  familyProblem,
  familyWrite,
  REASONING_LEVEL_LABELS,
  REASONING_LEVELS,
  takenModelNames,
  type FamilyDraft,
} from "@/lib/models-model";
import type { ModelFamily, ModelRoute, ReasoningLevel } from "@/lib/obleth";
import { Sheet } from "@/components/ui/sheet";

/** The family being edited: a new one, or one from the list. */
type Editing = { family: ModelFamily | null } | null;

/**
 * Model families on the Models page: one name callers send, with the
 * reasoning level they ask for choosing which chat model answers. Each is
 * saved on its own from a sheet, like adding a model.
 */
export function FamiliesSection({ families, models }: {
  /** Null when the gateway could not list them, such as one older than families. */
  families: ModelFamily[] | null;
  models: ModelRoute[];
}) {
  const [editing, setEditing] = useState<Editing>(null);
  return (
    <section aria-label="Model families" className="overflow-hidden rounded-xl border border-border bg-card">
      <header className="flex flex-wrap items-start justify-between gap-3 px-[18px] pb-3 pt-4">
        <div className="min-w-0">
          <h2 className="text-sm font-semibold">Model families</h2>
          <p className="mt-0.5 text-xs text-muted-foreground">
            One name for several chat models. Callers send the family&apos;s name as the model, and the reasoning effort they ask for decides which model answers, for example an instruct model for none and its thinking sibling for the rest.
          </p>
        </div>
        {families !== null && (
          <Button type="button" variant="outline" size="sm" onClick={() => setEditing({ family: null })}><Plus className="h-3.5 w-3.5" />Add family</Button>
        )}
      </header>
      {families === null ? (
        <p className="border-t border-border px-[18px] py-4 text-[13px] text-muted-foreground">The gateway did not list model families. It may be a version from before families.</p>
      ) : families.length === 0 ? (
        <p className="border-t border-border px-[18px] py-4 text-[13px] text-muted-foreground">No families yet.</p>
      ) : (
        <div className="overflow-x-auto border-t border-border">
          <table className="w-full min-w-[720px] text-[13px]">
            <thead>
              <tr className="border-b border-border text-left text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">
                <th className="py-2.5 pl-[18px] pr-3 font-semibold">Family</th>
                <th className="py-2.5 pr-3 font-semibold">No effort set</th>
                <th className="py-2.5 pr-[18px] font-semibold">Who answers</th>
              </tr>
            </thead>
            <tbody>
              {families.map((f) => (
                <tr key={f.id} onClick={() => setEditing({ family: f })} className="cursor-pointer border-b border-border/70 transition-colors last:border-b-0 hover:bg-muted/30">
                  <td className="max-w-[22rem] py-2.5 pl-[18px] pr-3">
                    <button type="button" onClick={(e) => { e.stopPropagation(); setEditing({ family: f }); }} className="font-mono text-[12.5px] font-medium hover:underline">{f.name}</button>
                    {f.description && <span className="block truncate text-[12px] text-muted-foreground" title={f.description}>{f.description}</span>}
                  </td>
                  <td className="whitespace-nowrap py-2.5 pr-3 text-secondary-foreground">served as {f.default_level}</td>
                  <td className="py-2.5 pr-[18px] font-mono text-[12px] text-secondary-foreground">
                    {familyGroups(f.members).map((g) => `${g.levels.join(", ")} → ${g.model}`).join(" · ")}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {editing && (
        <FamilySheet
          key={editing.family?.id ?? "new"}
          family={editing.family}
          families={families ?? []}
          models={models}
          onClose={() => setEditing(null)}
        />
      )}
    </section>
  );
}

function FamilySheet({ family, families, models, onClose }: {
  family: ModelFamily | null;
  families: ModelFamily[];
  models: ModelRoute[];
  onClose: () => void;
}) {
  const router = useRouter();
  const { confirm, confirmElement } = useConfirm();
  const [draft, setDraft] = useState<FamilyDraft>(() => familyDraft(family));
  const [tried, setTried] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, start] = useTransition();

  const taken = { modelNames: takenModelNames(models), familyNames: families.filter((f) => f.id !== family?.id).map((f) => f.name) };
  const problem = familyProblem(draft, taken);
  const preview = familyPreview(draft);
  const options = familyMemberOptions(models, Object.values(family?.members ?? {}));
  const setMember = (level: ReasoningLevel, name: string) => setDraft((d) => ({ ...d, members: { ...d.members, [level]: name } }));

  function save() {
    setTried(true);
    if (problem) return;
    start(async () => {
      const result = await saveModelFamilyAction(family?.id ?? null, familyWrite(draft));
      if (!result.ok) {
        setError(result.error);
        return;
      }
      router.refresh();
      onClose();
    });
  }

  function remove() {
    if (!family) return;
    start(async () => {
      const ok = await confirm({
        title: `Delete ${family.name}?`,
        description: "Requests naming it start failing straight away. Its models are not touched and still answer to their own names.",
        confirmLabel: "Delete",
      });
      if (!ok) return;
      const result = await deleteModelFamilyAction(family.id);
      if (!result.ok) {
        setError(result.error);
        return;
      }
      router.refresh();
      onClose();
    });
  }

  return (
    <Sheet
      open
      onClose={onClose}
      title={family ? `Edit ${family.name}` : "Add a model family"}
      description="Callers send this name as the model. The effort level they ask for picks which of your chat models answers."
      footer={
        <div className="flex items-center justify-between gap-2">
          <div>{family && <Button type="button" variant="outline" size="sm" disabled={busy} onClick={remove}>Delete family…</Button>}</div>
          <div className="flex items-center gap-2">
            <Button type="button" variant="ghost" size="sm" disabled={busy} onClick={onClose}>Cancel</Button>
            <Button type="button" size="sm" disabled={busy} onClick={save}>{busy ? "Saving…" : family ? "Save family" : "Add family"}</Button>
          </div>
        </div>
      }
    >
      {confirmElement}
      <div className="flex flex-col gap-4 px-6 pb-6">
        {(error || (tried && problem)) && <p role="alert" className="rounded-lg border border-foreground/60 px-3 py-2 text-[12.5px]">{error ?? problem}</p>}
        <Field
          label="Name"
          name="family_name"
          value={draft.name}
          onChange={(e) => setDraft((d) => ({ ...d, name: e.target.value }))}
          placeholder="acme-7b"
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
          className="[&_input]:font-mono"
          hint="The name callers put in the model field. It can't have spaces or be the name, alias or variant of any model."
        />
        <Field
          label="Description"
          name="family_description"
          value={draft.description}
          onChange={(e) => setDraft((d) => ({ ...d, description: e.target.value }))}
          placeholder="Instruct for quick answers, thinking for the rest."
          hint="Optional. A sentence for other operators about what the family is for."
        />
        <div className="space-y-1.5">
          <span className="text-[12.5px] font-medium text-secondary-foreground">Which model answers each level</span>
          <div className="flex flex-col divide-y divide-border rounded-lg border border-border">
            {REASONING_LEVELS.map((level) => (
              <div key={level} className="flex items-center gap-3 px-3 py-2">
                <span className="w-16 shrink-0 text-[13px] font-medium">{REASONING_LEVEL_LABELS[level]}</span>
                <div className="min-w-0 flex-1">
                  <SelectField label={`Model for ${level}`} value={draft.members[level]} onChange={(v) => setMember(level, v)} options={options} />
                </div>
              </div>
            ))}
          </div>
          <p className="text-[11.5px] leading-snug text-muted-foreground">Only chat models are offered. The model that answers is billed, queued and checked against the caller&apos;s allowlist as if they had named it, and gets the level passed on, so its own reasoning settings still apply.</p>
        </div>
        <div className="space-y-1.5">
          <span className="text-[12.5px] font-medium text-secondary-foreground">When a caller sets no effort</span>
          <div className="w-64">
            <SelectField
              label="When a caller sets no effort"
              value={draft.default_level}
              onChange={(v) => setDraft((d) => ({ ...d, default_level: v as ReasoningLevel }))}
              options={REASONING_LEVELS.map((l) => ({ value: l, label: `Serve it as ${l}` }))}
            />
          </div>
          <p className="text-[11.5px] leading-snug text-muted-foreground">The level a request is treated as when it names none, which also decides the model that answers it.</p>
        </div>
        <div className="rounded-lg bg-muted/50 px-3 py-2.5 text-[12.5px]">
          {preview ? (
            <>
              <p className="text-secondary-foreground">{preview.lead}</p>
              <p className="mt-1 font-mono text-[12px] text-muted-foreground">{preview.levels}</p>
            </>
          ) : (
            <p className="text-muted-foreground">Pick a model for every level to see where requests go.</p>
          )}
        </div>
      </div>
    </Sheet>
  );
}
