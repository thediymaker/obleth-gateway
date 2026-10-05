"use client";

import { useState, useTransition } from "react";
import Link from "next/link";
import { setModelStatusAction } from "@/app/actions";
import { SelectField, Setting, Switch, TextArea } from "@/components/models/fields";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { logsHref } from "@/lib/log-links";
import { dateInputValue, lifecycleDate, lifecycleStatus, retireAtFromInput, retiredMessage } from "@/lib/models-model";
import type { LifecycleStatus, ModelRoute, UsageLogFacet } from "@/lib/obleth";

/** A model the replacement picker may offer. */
export interface LifecycleCandidate {
  name: string;
  type: string;
  status: LifecycleStatus;
}

const STATUSES: { value: LifecycleStatus; label: string; hint: string }[] = [
  {
    value: "staged",
    label: "Staged",
    hint: "Answers anyone who sends its name, but it is left out of /v1/models, /model/info and the portal, and auto never picks it. Test it in the Playground or with your own key, then make it active.",
  },
  { value: "active", label: "Active", hint: "Served as usual, and listed for everyone who may call it." },
  {
    value: "deprecated",
    label: "Deprecated",
    hint: "Still served, but every response says it is going away, when, and what to use instead. Auto stops picking it. On the retirement date it retires itself.",
  },
  {
    value: "retired",
    label: "Retired",
    hint: "No longer served. Callers get a 410 error that names the replacement, or the replacement answers for it if you turn that on below. It leaves the model list.",
  },
];

/** The models that may replace `model`: same type, someone else, still served and listed. */
export function replacementOptions(model: Pick<ModelRoute, "model_name" | "model_type">, models: LifecycleCandidate[]) {
  return models
    .filter((m) => m.name !== model.model_name && m.type === model.model_type && (m.status === "active" || m.status === "deprecated"))
    .map((m) => ({ value: m.name, label: m.status === "deprecated" ? `${m.name} (deprecated)` : m.name }))
    .sort((a, b) => a.value.localeCompare(b.value));
}

/**
 * Where the model is in its life, and who would notice a change: the keys
 * that called it in the last 30 days. Saved on its own, at once, like the
 * endpoints below it: a status is not one of the model's settings.
 */
export function LifecycleSection({
  model,
  models,
  callers,
  onChanged,
}: {
  model: ModelRoute;
  models: LifecycleCandidate[];
  /** Keys that called the model in the last 30 days, busiest first; null when the logs could not be read. */
  callers: UsageLogFacet[] | null;
  onChanged: () => void;
}) {
  const stored = model.lifecycle ?? { status: "active" as const };
  const [status, setStatus] = useState<LifecycleStatus>(stored.status);
  const [replacement, setReplacement] = useState(stored.replacement ?? "");
  const [date, setDate] = useState(dateInputValue(stored.retire_at));
  const [note, setNote] = useState(stored.note ?? "");
  const [redirect, setRedirect] = useState(!!stored.redirect);
  const [error, setError] = useState<string | null>(null);
  const [busy, start] = useTransition();
  const { confirm, confirmElement } = useConfirm();

  const effective = lifecycleStatus(model);
  const options = replacementOptions(model, models);
  const dirty =
    status !== stored.status ||
    replacement !== (stored.replacement ?? "") ||
    date !== dateInputValue(stored.retire_at) ||
    note !== (stored.note ?? "") ||
    redirect !== !!stored.redirect;
  const leaving = status === "deprecated" || status === "retired";
  const retireAt = retireAtFromInput(date);
  const pastDate = !!retireAt && new Date(retireAt).getTime() <= Date.now();

  function reset() {
    setStatus(stored.status);
    setReplacement(stored.replacement ?? "");
    setDate(dateInputValue(stored.retire_at));
    setNote(stored.note ?? "");
    setRedirect(!!stored.redirect);
    setError(null);
  }

  function save() {
    start(async () => {
      const retiringNow = (status === "retired" || (status === "deprecated" && pastDate)) && effective !== "retired";
      if (retiringNow) {
        const ok = await confirm({
          title: `Retire ${model.model_name} now?`,
          description: redirect && replacement
            ? `Requests naming it are answered by ${replacement} from now on.`
            : "Requests naming it start failing with a 410 error straight away.",
          confirmLabel: "Retire",
        });
        if (!ok) return;
      }
      if (stored.status === "staged" && status === "active") {
        const ok = await confirm({
          title: `Make ${model.model_name} live?`,
          description: model.auto_eligible
            ? "It appears in /v1/models and the portal, and auto can start sending it requests."
            : "It appears in /v1/models and the portal. Auto still skips it, as its routing settings say.",
          confirmLabel: "Make live",
        });
        if (!ok) return;
      }
      const result = await setModelStatusAction(model.id, {
        status,
        replacement: leaving ? replacement || null : null,
        retire_at: leaving ? retireAt : null,
        note: leaving ? note : null,
        redirect: leaving && redirect && !!replacement,
      });
      if (!result.ok) {
        setError(result.error);
        return;
      }
      setError(null);
      onChanged();
    });
  }

  return (
    <section id="lifecycle" data-section="lifecycle" aria-label="Lifecycle" className="scroll-mt-24 rounded-xl border border-border bg-card">
      {confirmElement}
      <header className="flex flex-wrap items-start justify-between gap-3 px-[18px] pb-3 pt-4">
        <div>
          <h2 className="text-sm font-semibold">Lifecycle</h2>
          <p className="mt-0.5 text-xs text-muted-foreground">
            Stage a new model to test it before anyone can find it, then make it active. Take one out of service without surprising anyone: deprecate it with a date and a replacement, then let it retire. Saved on its own, and applies within a few seconds.
          </p>
        </div>
        <div className="flex items-center gap-2">
          {dirty && <Button type="button" size="sm" variant="ghost" onClick={reset} disabled={busy}>Discard</Button>}
          <Button type="button" size="sm" onClick={save} disabled={!dirty || busy}>{busy ? "Saving…" : "Save lifecycle"}</Button>
        </div>
      </header>
      {error && <p role="alert" className="mx-[18px] mb-3 rounded-lg border border-foreground/60 px-3 py-2 text-[12.5px]">{error}</p>}

      <Setting label="Status" hint={STATUSES.find((s) => s.value === status)?.hint}>
        <span role="radiogroup" aria-label="Lifecycle status" className="inline-flex items-center gap-0.5 self-start rounded-lg border border-border p-[3px]">
          {STATUSES.map((s) => (
            <label key={s.value} className="cursor-pointer">
              <input type="radio" name="lifecycle_status" value={s.value} checked={status === s.value} onChange={() => setStatus(s.value)} className="peer sr-only" />
              <span className="inline-flex h-7 items-center rounded-md px-2.5 text-[12.5px] text-muted-foreground transition-colors hover:text-foreground peer-checked:bg-secondary peer-checked:text-foreground peer-focus-visible:ring-1 peer-focus-visible:ring-ring">
                {s.label}
              </span>
            </label>
          ))}
        </span>
        {stored.status === "deprecated" && effective === "retired" && status === "deprecated" && (
          <span className="text-[12px] text-muted-foreground">Its retirement date has passed, so it is already retired.</span>
        )}
      </Setting>

      {status === "staged" && (
        <Setting label="What callers see" hint="Only people you give the name to can reach it.">
          <p className="text-[12.5px] text-secondary-foreground">
            A request that names <span className="font-mono">{model.model_name}</span> is served as usual. Looking it up with{" "}
            <span className="font-mono">GET /v1/models/{model.model_name}</span> answers 404, as if it did not exist.
          </p>
        </Setting>
      )}

      {leaving && (
        <>
          <Setting label="Replacement" hint="The model callers are told to move to. Only models of the same type that are still served are offered.">
            <div className="w-72">
              <SelectField label="Replacement" value={replacement} onChange={setReplacement} options={[{ value: "", label: "None" }, ...options]} />
            </div>
          </Setting>
          <Setting
            label={status === "deprecated" ? "Retire on" : "Retired on"}
            hint={
              status === "deprecated"
                ? "The day it stops being served, at midnight UTC. Callers see this date in every response until then, and it retires itself that day."
                : "The day callers are told it was retired. Blank uses the day you save this."
            }
          >
            <input
              type="date"
              aria-label={status === "deprecated" ? "Retire on" : "Retired on"}
              value={date}
              onChange={(e) => setDate(e.target.value)}
              className="h-9 w-44 rounded-md border border-input bg-transparent px-3 text-[13px]"
            />
            {status === "deprecated" && pastDate && <span className="text-[12px] text-muted-foreground">That date has passed: saving this retires it now.</span>}
          </Setting>
          <Setting label="Note to callers" hint="A sentence added after the status, such as why it is going or where to ask. Up to 500 characters.">
            <TextArea name="lifecycle_note" label="Note to callers" rows={2} value={note} onChange={setNote} placeholder="Questions? Write to the research computing team." className="font-sans text-[12.5px]" />
          </Setting>
          <Setting label="Once retired" hint="Off: callers get a 410 error naming the replacement. On: the replacement answers instead, and the response says it was redirected. A different model can answer differently, so leave this off unless the replacement is a close match.">
            <Switch label="Answer with the replacement" checked={redirect && !!replacement} disabled={!replacement} onChange={setRedirect}>
              {replacement ? `Answer with ${replacement}` : "Pick a replacement first"}
            </Switch>
          </Setting>
          <Setting label="What callers see" hint={status === "deprecated" ? "On every response, in standard headers client libraries can read." : "The error a request gets once it is retired."}>
            {status === "deprecated" && !pastDate ? (
              <ul className="flex flex-col gap-1 font-mono text-[12px] text-secondary-foreground">
                <li>x-obleth-model-status: deprecated</li>
                <li>Deprecation: {stored.status === "deprecated" && stored.changed_at ? lifecycleDate(stored.changed_at) : "the day you save this"}</li>
                {retireAt && <li>Sunset: {lifecycleDate(retireAt)}</li>}
                {replacement && <li>Link: /v1/models/{replacement}; rel=&quot;successor-version&quot;</li>}
              </ul>
            ) : redirect && replacement ? (
              <p className="text-[12.5px] text-secondary-foreground">Answers from <span className="font-mono">{replacement}</span>, with <span className="font-mono">x-obleth-redirected-from: {model.model_name}</span> on the response.</p>
            ) : (
              <p className="rounded-md bg-muted/50 px-3 py-2 font-mono text-[12px] text-secondary-foreground">
                410 · {retiredMessage(model.model_name, { status: "retired", replacement, retire_at: retireAt ?? undefined, changed_at: (stored.status === "retired" ? stored.changed_at : null) ?? new Date().toISOString(), note: note.trim() })}
              </p>
            )}
          </Setting>
        </>
      )}

      <Setting label="Who calls it" hint="Keys that sent it requests in the last 30 days. Tell them before you retire it.">
        {callers === null ? (
          <span className="text-[12.5px] text-muted-foreground">The request logs could not be read.</span>
        ) : callers.length === 0 ? (
          <span className="text-[12.5px] text-muted-foreground">No requests in the last 30 days.</span>
        ) : (
          <ul className="flex flex-col gap-1 text-[12.5px]">
            {callers.slice(0, 8).map((c) => (
              <li key={c.value} className="flex items-baseline justify-between gap-4">
                <Link href={logsHref({ model: model.model_name, key: c.value, window: "30d" })} className="truncate hover:underline">{c.label || c.value}</Link>
                <span className="shrink-0 tabular-nums text-muted-foreground">{c.requests.toLocaleString()} {c.requests === 1 ? "request" : "requests"}</span>
              </li>
            ))}
            {callers.length > 8 && (
              <li><Link href={logsHref({ model: model.model_name, window: "30d" })} className="text-muted-foreground hover:text-foreground">and {callers.length - 8} more ›</Link></li>
            )}
          </ul>
        )}
      </Setting>
    </section>
  );
}
