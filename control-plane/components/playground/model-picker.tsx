"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { Braces, Check, Eye, Search, Wrench } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import type { FairshareLiveView, ModelRoute } from "@/lib/obleth";
import { cn } from "@/lib/utils";
import { ModelAvatar, Pill, SectionLabel, capabilities, formatTokens, modelLabel, perMillion } from "./ui";

export const MAX_LANES = 4;

type Filter = "all" | "chat" | "image" | "vision" | "tools";

interface Entry {
  name: string;
  label: string;
  detail: string;
  kind: "special" | "chat" | "image";
  model?: ModelRoute;
}

/** Live in-flight counts per model, read once each time the picker opens. */
function useLoad(open: boolean) {
  const [load, setLoad] = useState<{ inFlight: Record<string, number>; defaultCap?: number } | null>(null);
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    fetch("/api/live/fairshare")
      .then((r) => (r.ok ? (r.json() as Promise<FairshareLiveView>) : null))
      .then((view) => { if (!cancelled && view) setLoad({ inFlight: view.model_in_flight ?? {}, defaultCap: view.default_model_max_in_flight }); })
      .catch(() => { /* load is a nicety; the picker works without it */ });
    return () => { cancelled = true; };
  }, [open]);
  return load;
}

/**
 * Choose the model(s) a chat session talks to. One highlighted row fills the
 * detail pane; ticking boxes builds a comparison of up to four.
 */
export function ModelPicker({ open, onOpenChange, models, selected, locked, onChoose }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  models: ModelRoute[];
  /** Names currently receiving messages; seeds the checkboxes. */
  selected: string[];
  /** How many more lanes can be added before the session is full. */
  locked?: boolean;
  onChoose: (names: string[]) => void;
}) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [ticked, setTicked] = useState<string[]>(selected);
  const [highlight, setHighlight] = useState<string>(selected[0] ?? "auto");
  const list = useRef<HTMLDivElement>(null);
  const load = useLoad(open);

  useEffect(() => {
    if (open) { setTicked(selected); setHighlight(selected[0] ?? "auto"); setQuery(""); setFilter("all"); }
    // Reseed only when the dialog opens, not on every parent render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const entries = useMemo<Entry[]>(() => {
    const special: Entry[] = [
      { name: "auto", label: "Automatic routing", detail: "Picks a model per request using the router weights", kind: "special" },
      { name: "charo", label: "Assistant", detail: "Guided capability tests and benchmarks", kind: "special" },
    ];
    const registry = models
      .filter((m) => ["chat", "image"].includes(m.model_type) && !["auto", "charo"].includes(m.model_name))
      .map<Entry>((m) => ({ name: m.model_name, label: m.model_name, detail: m.model_type === "image" ? "Image model" : "Chat model", kind: m.model_type === "image" ? "image" : "chat", model: m }));
    return [...special, ...registry];
  }, [models]);

  const visible = entries.filter((e) => {
    const q = query.trim().toLowerCase();
    if (q && !`${e.label} ${e.name} ${e.model?.tags.join(" ") ?? ""}`.toLowerCase().includes(q)) return false;
    if (filter === "all") return true;
    if (filter === "chat") return e.kind === "chat";
    if (filter === "image") return e.kind === "image";
    if (filter === "vision") return !!e.model?.supports_vision;
    return !!e.model?.supports_function_calling;
  });
  const sections: [string, Entry[]][] = [
    ["Let the gateway decide", visible.filter((e) => e.kind === "special")],
    ["Chat models", visible.filter((e) => e.kind === "chat")],
    ["Image models", visible.filter((e) => e.kind === "image")],
  ];
  const flat = sections.flatMap(([, items]) => items);
  const current = entries.find((e) => e.name === highlight) ?? flat[0];

  const toggle = (name: string) => setTicked((t) => (t.includes(name) ? t.filter((n) => n !== name) : t.length >= MAX_LANES ? t : [...t, name]));
  const choose = (names: string[]) => { onChoose(names); onOpenChange(false); };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (!flat.length) return;
    const at = Math.max(0, flat.findIndex((x) => x.name === current?.name));
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const next = flat[(at + (e.key === "ArrowDown" ? 1 : flat.length - 1)) % flat.length];
      setHighlight(next.name);
      list.current?.querySelector<HTMLElement>(`[data-name="${CSS.escape(next.name)}"]`)?.focus();
    } else if (e.key === " " && (e.target as HTMLElement).dataset.name) {
      e.preventDefault();
      toggle(flat[at].name);
    }
  };

  const loadText = (m?: ModelRoute) => {
    if (!m || !load) return null;
    const n = load.inFlight[m.model_name] ?? 0;
    return n === 0 ? "Idle" : `${n} busy`;
  };
  const cap = (m?: ModelRoute) => m?.max_in_flight ?? load?.defaultCap;
  const multi = ticked.length >= 2;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="flex h-[min(640px,calc(100vh-2rem))] max-w-[820px] gap-0 overflow-hidden rounded-2xl p-0">
        <DialogTitle className="sr-only">Choose models</DialogTitle>
        <DialogDescription className="sr-only">Pick one model to chat with, or tick up to four to compare side by side.</DialogDescription>
        <div className="flex min-w-0 flex-1 flex-col border-border pb-16 md:max-w-[480px] md:border-r md:pb-0">
          <label className="flex h-[52px] shrink-0 items-center gap-2.5 border-b border-border pl-4 pr-12 text-muted-foreground">
            <Search className="h-4 w-4 shrink-0" aria-hidden="true" />
            <input
              autoFocus
              aria-label="Search models"
              placeholder="Search by name or tag"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => { if (e.key === "ArrowDown") { e.preventDefault(); list.current?.querySelector<HTMLElement>("[data-name]")?.focus(); } if (e.key === "Enter" && flat[0]) choose([flat[0].name]); }}
              className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground"
            />
          </label>
          <div role="group" aria-label="Filter models" className="flex flex-wrap gap-1 px-3 pb-1.5 pt-2.5">
            {(["all", "chat", "image", "vision", "tools"] as Filter[]).map((f) => (
              <button key={f} type="button" aria-pressed={filter === f} onClick={() => setFilter(f)}
                className={cn("h-7 rounded-md px-2.5 text-[12.5px] capitalize", filter === f ? "bg-secondary text-foreground" : "text-muted-foreground hover:text-foreground")}>
                {f}
              </button>
            ))}
          </div>
          <div ref={list} aria-label="Models" onKeyDown={onKeyDown} className="min-h-0 flex-1 overflow-y-auto px-2 py-1">
            {flat.length === 0 && <p className="px-3 py-6 text-center text-sm text-muted-foreground">No models match.</p>}
            {sections.filter(([, items]) => items.length).map(([title, items]) => (
              <div key={title} className="pb-2">
                <SectionLabel className="px-2.5 pb-1 pt-2">{title}</SectionLabel>
                {items.map((e) => {
                  const on = ticked.includes(e.name);
                  return (
                    <div key={e.name} className={cn("flex items-center gap-2.5 rounded-lg px-2.5 py-[7px]", current?.name === e.name ? "bg-secondary" : "hover:bg-accent/60")}>
                      <button
                        type="button"
                        role="checkbox"
                        aria-checked={on}
                        aria-label={`Add ${e.label} to comparison`}
                        disabled={!on && (ticked.length >= MAX_LANES || !!locked)}
                        onClick={() => toggle(e.name)}
                        className={cn("flex h-4 w-4 shrink-0 items-center justify-center rounded border-[1.5px] disabled:opacity-40", on ? "border-foreground bg-foreground text-background" : "border-muted-foreground/60")}
                      >
                        {on && <Check className="h-3 w-3" strokeWidth={3} />}
                      </button>
                      <button
                        type="button"
                        aria-current={current?.name === e.name ? "true" : undefined}
                        data-name={e.name}
                        onFocus={() => setHighlight(e.name)}
                        onMouseEnter={() => setHighlight(e.name)}
                        onClick={() => setHighlight(e.name)}
                        onDoubleClick={() => choose([e.name])}
                        onKeyDown={(k) => { if (k.key === "Enter") { k.preventDefault(); choose([e.name]); } }}
                        className="flex min-w-0 flex-1 items-center gap-2.5 text-left focus-visible:outline-none"
                      >
                        <ModelAvatar name={e.name} />
                        <span className="min-w-0 flex-1">
                          <span className="block truncate text-[13px]">
                            {e.label}
                            {e.kind === "special" && e.name === "auto" && <span className="ml-1.5 font-mono text-[11.5px] text-muted-foreground">auto</span>}
                            {selected.includes(e.name) && <span className="ml-1.5 text-[11.5px] text-muted-foreground">current</span>}
                          </span>
                          {e.kind === "special" && <span className="block truncate text-[11.5px] text-muted-foreground">{e.detail}</span>}
                        </span>
                        {e.kind === "chat" && <span className="w-10 text-right font-mono text-[11.5px] text-muted-foreground">{formatTokens(e.model!.context_window)}</span>}
                        {e.kind === "chat" && e.model && (
                          <span className="hidden gap-0.5 sm:flex" aria-hidden="true">
                            <Wrench className={cn("h-3.5 w-3.5", e.model.supports_function_calling ? "text-muted-foreground" : "text-muted/80")} />
                            <Braces className={cn("h-3.5 w-3.5", e.model.supports_response_schema ? "text-muted-foreground" : "text-muted/80")} />
                            <Eye className={cn("h-3.5 w-3.5", e.model.supports_vision ? "text-muted-foreground" : "text-muted/80")} />
                          </span>
                        )}
                        {e.model && <span className="w-12 text-right text-[11.5px] text-muted-foreground">{loadText(e.model)}</span>}
                      </button>
                    </div>
                  );
                })}
              </div>
            ))}
          </div>
          <div className="hidden shrink-0 items-center gap-3 border-t border-border px-3.5 py-2.5 text-[11.5px] text-muted-foreground sm:flex">
            <span><Kbd>↑</Kbd> <Kbd>↓</Kbd> move</span>
            <span><Kbd>space</Kbd> add to compare</span>
            <span><Kbd>enter</Kbd> chat</span>
            {locked && <span className="ml-auto">Session is full — start a new one to compare others</span>}
          </div>
        </div>

        <div className="hidden w-[340px] shrink-0 flex-col gap-5 p-5 md:flex">
          {current && <Detail entry={current} inFlight={current.model ? load?.inFlight[current.model.model_name] : undefined} cap={cap(current.model)} />}
          <div className="flex-1" />
          <div className="space-y-2">
            {multi && (
              <Button className="h-10 w-full" onClick={() => choose(ticked)}>Compare {ticked.length} models side by side</Button>
            )}
            {current && (
              <Button variant={multi ? "outline" : "default"} className="h-10 w-full" onClick={() => choose([current.name])}>
                <span className="truncate">Chat with {current.label}{multi ? " only" : ""}</span>
              </Button>
            )}
          </div>
        </div>
        {/* Narrow screens have no detail pane; keep the actions reachable. */}
        <div className="absolute inset-x-0 bottom-0 flex gap-2 border-t border-border bg-card p-3 md:hidden">
          {multi && <Button className="flex-1" onClick={() => choose(ticked)}>Compare {ticked.length}</Button>}
          {current && <Button variant={multi ? "outline" : "default"} className="min-w-0 flex-1" onClick={() => choose([current.name])}><span className="truncate">Chat with {current.label}</span></Button>}
        </div>
      </DialogContent>
    </Dialog>
  );
}

function Kbd({ children }: { children: React.ReactNode }) {
  return <kbd className="rounded border border-border px-1 font-mono text-[11px] text-secondary-foreground">{children}</kbd>;
}

function Detail({ entry, inFlight, cap }: { entry: Entry; inFlight?: number; cap?: number | null }) {
  const m = entry.model;
  return (
    <>
      <div className="flex items-center gap-3">
        <ModelAvatar name={entry.name} size="lg" />
        <div className="min-w-0">
          <div className="truncate text-[15px] font-semibold">{modelLabel(entry.name)}</div>
          <div className="text-xs text-muted-foreground">{entry.detail}</div>
        </div>
      </div>
      {m && (
        <div className="grid grid-cols-2 gap-x-4 gap-y-3 text-xs">
          {m.model_type === "image" ? (
            <Stat label="Price" value={m.cost_per_image ? `$${+m.cost_per_image.toPrecision(3)} / image` : "free"} />
          ) : (
            <>
              <Stat label="Context" value={`${formatTokens(m.context_window)} tokens`} />
              <Stat label="Type" value="Chat" plain />
              <Stat label="Input" value={`${perMillion(m.input_cost_per_token)} / 1M`} />
              <Stat label="Output" value={`${perMillion(m.output_cost_per_token)} / 1M`} />
            </>
          )}
        </div>
      )}
      {m && m.model_type !== "image" && (
        <div className="space-y-2">
          <SectionLabel>Capabilities</SectionLabel>
          <div className="space-y-1.5 text-[12.5px]">
            {capabilities(m).map((c) => (
              <div key={c.key} className={cn("flex justify-between", !c.on && "text-muted-foreground")}>
                <span>{c.label}</span><span>{c.on ? "Yes" : "No"}</span>
              </div>
            ))}
          </div>
        </div>
      )}
      {m && m.tags.length > 0 && (
        <div className="space-y-2">
          <SectionLabel>Router tags</SectionLabel>
          <div className="flex flex-wrap gap-1.5">{m.tags.map((t) => <Pill key={t}>{t}</Pill>)}</div>
        </div>
      )}
      {m && inFlight !== undefined && (
        <div className="space-y-2">
          <div className="flex justify-between"><SectionLabel>Load now</SectionLabel><span className="text-xs text-muted-foreground">{cap ? `${inFlight} of ${cap} slots` : `${inFlight} in flight`}</span></div>
          {cap ? (
            cap <= 16 ? (
              <div className="flex gap-[3px]" aria-hidden="true">
                {Array.from({ length: cap }, (_, i) => <span key={i} className={cn("h-2 flex-1 rounded-sm", i < inFlight ? "bg-foreground" : "bg-muted")} />)}
              </div>
            ) : (
              <div className="h-2 overflow-hidden rounded-sm bg-muted" aria-hidden="true"><div className="h-2 bg-foreground" style={{ width: `${Math.min(100, (inFlight / cap) * 100)}%` }} /></div>
            )
          ) : null}
        </div>
      )}
    </>
  );
}

function Stat({ label, value, plain }: { label: string; value: string; plain?: boolean }) {
  return (
    <div>
      <div className="text-muted-foreground">{label}</div>
      <div className={cn("text-[13px]", !plain && "font-mono")}>{value}</div>
    </div>
  );
}
