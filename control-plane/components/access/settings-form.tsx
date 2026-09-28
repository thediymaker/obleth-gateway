"use client";

import { useCallback, useEffect, useMemo, useRef, useState, useTransition, type ReactNode } from "react";
import { ChangesContext } from "@/components/models/fields";
import { Button } from "@/components/ui/button";
import { snapshotForm, type FormSnapshot } from "@/lib/models-model";
import { cn } from "@/lib/utils";

export type SaveResult = { ok: true } | { ok: false; error: string; saved: string[] };

/**
 * One form, one save bar. It snapshots the form as first painted, watches
 * every field (typed, clicked, or changed by a control without an input
 * event), and marks each changed setting through `ChangesContext`. Save
 * sends the whole form with `id` and the changed `sections`; whatever saved
 * becomes the new baseline, and a refusal leaves the rest marked. Discard
 * remounts the form from the stored values. Leaving with changes asks first.
 */
export function SettingsForm({
  id,
  sectionOf,
  labelOf,
  save,
  validate,
  onSaved,
  onDirtyChange,
  children,
  bar = "floating",
  className,
  ariaLabel,
}: {
  id: string;
  /** Which save call owns a field; null for markers and fields that never save. */
  sectionOf: (field: string) => string | null;
  /** What the save bar calls a changed field. */
  labelOf: (field: string) => string;
  save: (data: FormData) => Promise<SaveResult>;
  /** A check the browser can't make; its message stops the save. */
  validate?: (data: FormData) => string | null;
  onSaved?: (data: FormData) => void;
  onDirtyChange?: (sections: string[], count: number) => void;
  children: ReactNode;
  /** `floating` sits over the page's bottom edge; `panel` is a footer inside a panel. */
  bar?: "floating" | "panel";
  className?: string;
  ariaLabel?: string;
}) {
  const form = useRef<HTMLFormElement>(null);
  const [version, setVersion] = useState(0);
  const [baseline, setBaseline] = useState<FormSnapshot | null>(null);
  const [changed, setChanged] = useState<string[]>([]);
  const [pending, start] = useTransition();
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const frame = useRef(0);
  const baseRef = useRef<FormSnapshot | null>(null);
  baseRef.current = baseline;

  const read = useCallback(() => (form.current ? snapshotForm(new FormData(form.current)) : null), []);
  const diff = useCallback(
    (base: FormSnapshot, next: FormSnapshot) =>
      [...new Set([...base.keys(), ...next.keys()])].filter((n) => sectionOf(n) !== null && (base.get(n) ?? "") !== (next.get(n) ?? "")).sort(),
    [sectionOf],
  );
  const recompute = useCallback(() => {
    cancelAnimationFrame(frame.current);
    frame.current = requestAnimationFrame(() => {
      const next = read();
      const base = baseRef.current;
      if (!next || !base) return;
      const fields = diff(base, next);
      setChanged((prev) => (prev.join("|") === fields.join("|") ? prev : fields));
    });
  }, [read, diff]);

  useEffect(() => {
    setBaseline(read());
    setChanged([]);
  }, [read, version]);

  useEffect(() => {
    const el = form.current;
    if (!el) return;
    const observer = new MutationObserver(recompute);
    observer.observe(el, { subtree: true, childList: true, attributes: true, attributeFilter: ["value", "checked"] });
    return () => observer.disconnect();
  }, [recompute, version]);

  const sections = useMemo(() => [...new Set(changed.map(sectionOf).filter((x): x is string => !!x))], [changed, sectionOf]);
  const labels = useMemo(() => [...new Set(changed.map(labelOf))], [changed, labelOf]);
  useEffect(() => onDirtyChange?.(sections, changed.length), [sections, changed.length, onDirtyChange]);

  useEffect(() => {
    if (changed.length === 0) return;
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    const onClick = (e: MouseEvent) => {
      const a = e.target instanceof Element ? e.target.closest("a") : null;
      if (!a || a.target === "_blank" || e.metaKey || e.ctrlKey || e.shiftKey) return;
      const href = a.getAttribute("href") ?? "";
      if (!href || href.startsWith("#")) return;
      if (!window.confirm("You have unsaved changes. Leave without saving them?")) {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    document.addEventListener("click", onClick, true);
    return () => {
      window.removeEventListener("beforeunload", onBeforeUnload);
      document.removeEventListener("click", onClick, true);
    };
  }, [changed.length]);

  useEffect(() => {
    if (!result?.ok) return;
    const t = setTimeout(() => setResult(null), 2500);
    return () => clearTimeout(t);
  }, [result]);

  function submit() {
    const el = form.current;
    if (!el || sections.length === 0 || !el.reportValidity()) return;
    const data = new FormData(el);
    data.set("id", id);
    data.set("sections", sections.join(","));
    const problem = validate?.(data);
    if (problem) {
      setResult({ ok: false, text: problem });
      return;
    }
    const submitted = snapshotForm(data);
    setResult(null);
    start(async () => {
      const res = await save(data);
      const done = res.ok ? sections : res.saved;
      setBaseline((base) => {
        if (!base) return base;
        const next = new Map(base);
        for (const name of new Set([...base.keys(), ...submitted.keys()])) {
          const section = sectionOf(name);
          if (!section || !done.includes(section)) continue;
          if (submitted.has(name)) next.set(name, submitted.get(name)!);
          else next.delete(name);
        }
        return next;
      });
      setResult(res.ok ? { ok: true, text: "Saved." } : { ok: false, text: res.saved.length ? `${res.error} The other changes were saved.` : res.error });
      recompute();
      if (res.ok || res.saved.length) onSaved?.(data);
    });
  }

  const context = useMemo(() => ({ changed, initial: baseline }), [changed, baseline]);
  const show = changed.length > 0 || result;

  return (
    <ChangesContext.Provider value={context}>
      <form
        key={version}
        ref={form}
        aria-label={ariaLabel}
        onSubmit={(e) => { e.preventDefault(); submit(); }}
        onInput={recompute}
        onChange={recompute}
        onClick={recompute}
        onKeyUp={recompute}
        className={className}
      >
        {children}
      </form>
      {show && (
        <div
          role="region"
          aria-label="Unsaved changes"
          className={cn(
            "z-30 flex flex-wrap items-center justify-between gap-3 bg-card",
            bar === "floating" ? "sticky bottom-4 rounded-xl border border-muted-foreground/50 px-4 py-3 shadow-2xl" : "sticky bottom-0 border-t border-border px-6 py-3",
          )}
        >
          <div className="flex min-w-0 flex-wrap items-center gap-3 text-[13px]">
            {changed.length > 0 && (
              <>
                <span className="inline-flex h-[22px] items-center rounded-full bg-foreground px-2 text-[11.5px] font-semibold text-background">{labels.length} unsaved</span>
                <span className="min-w-0 truncate text-secondary-foreground">{labels.join(" · ")}</span>
              </>
            )}
            {result && <span role="status" className={cn(result.ok ? "text-secondary-foreground" : "font-medium text-foreground")}>{result.text}</span>}
          </div>
          <div className="flex items-center gap-2">
            {result && !result.ok && changed.length === 0 && <Button type="button" size="sm" variant="ghost" onClick={() => setResult(null)}>Dismiss</Button>}
            {changed.length > 0 && (
              <>
                <Button type="button" size="sm" variant="outline" disabled={pending} onClick={() => { setResult(null); setVersion((v) => v + 1); }}>Discard</Button>
                <Button type="button" size="sm" disabled={pending} onClick={submit}>{pending ? "Saving…" : "Save changes"}</Button>
              </>
            )}
          </div>
        </div>
      )}
    </ChangesContext.Provider>
  );
}
