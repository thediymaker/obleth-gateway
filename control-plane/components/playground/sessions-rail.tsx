"use client";

import { useMemo, useRef, useState } from "react";
import { Plus, Search, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { PlaygroundSession } from "./playground";
import { modeInfo, modelLabel, shortAgo } from "./ui";

const DAY = 86_400_000;

/** Today / Yesterday / This week / Older, by when a session was last touched. */
function groupOf(ts: number | undefined, now: Date): string {
  if (!ts) return "Older";
  const midnight = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  if (ts >= midnight) return "Today";
  if (ts >= midnight - DAY) return "Yesterday";
  if (ts >= midnight - 6 * DAY) return "This week";
  return "Older";
}

function subtitle(s: PlaygroundSession, now: number): string {
  const mode = modeInfo(s.mode).label;
  let detail = "";
  if (s.launcher) detail = "Choose what to try";
  else if (s.mode === "image") detail = s.imageModel ?? "";
  else if (s.mode === "router") detail = s.routerPrompt?.trim() ? "auto" : "";
  else if (s.mode === "verdicts") detail = `${s.verdictQuestions?.length ?? 1} question${(s.verdictQuestions?.length ?? 1) === 1 ? "" : "s"}`;
  else {
    const lanes = (s.recipients ?? s.models.map((_, i) => i)).map((i) => s.models[i]).filter(Boolean);
    detail = lanes.length > 1 ? `${lanes.length} models` : modelLabel(lanes[0] ?? "");
  }
  return [s.launcher ? "New" : mode, detail, shortAgo(s.updatedAt, now)].filter(Boolean).join(" · ");
}

export function SessionsRail({ sessions, active, onSelect, onCreate, onRemove, onImport, canCreate }: {
  sessions: PlaygroundSession[];
  active: string;
  onSelect: (id: string) => void;
  onCreate: () => void;
  onRemove: (id: string) => void;
  onImport: (file: File) => void;
  canCreate: boolean;
}) {
  const [query, setQuery] = useState("");
  const picker = useRef<HTMLInputElement>(null);
  const now = Date.now();
  const groups = useMemo(() => {
    const q = query.trim().toLowerCase();
    const visible = sessions
      .filter((s) => !q || s.title.toLowerCase().includes(q))
      .sort((a, b) => (b.updatedAt ?? 0) - (a.updatedAt ?? 0));
    const out = new Map<string, PlaygroundSession[]>();
    const today = new Date(now);
    for (const s of visible) {
      const g = groupOf(s.updatedAt, today);
      out.set(g, [...(out.get(g) ?? []), s]);
    }
    return [...out.entries()];
    // `now` changes every render; grouping only needs to follow the data.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessions, query]);

  return (
    <aside aria-label="Sessions" className="flex max-h-56 shrink-0 flex-col border-b border-border bg-card md:max-h-none md:w-64 md:border-b-0 md:border-r">
      <div className="space-y-3 p-3.5 pb-3">
        <Button className="h-9 w-full" onClick={onCreate} disabled={!canCreate}>
          <Plus className="h-4 w-4" />New session
        </Button>
        <label className="flex h-[34px] items-center gap-2 rounded-lg border border-border px-2.5 text-muted-foreground focus-within:ring-1 focus-within:ring-ring">
          <Search className="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
          <input
            aria-label="Search sessions"
            placeholder="Search sessions"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="min-w-0 flex-1 bg-transparent text-[13px] text-foreground outline-none placeholder:text-muted-foreground"
          />
        </label>
      </div>
      <nav aria-label="Saved sessions" className="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
        {groups.length === 0 && <p className="px-2 py-3 text-xs text-muted-foreground">No sessions match “{query}”.</p>}
        {groups.map(([group, items]) => (
          <div key={group} className="mb-2">
            <div className="px-2 pb-1.5 pt-2 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">{group}</div>
            <div className="space-y-0.5">
              {items.map((s) => {
                const Icon = modeInfo(s.mode).icon;
                const current = s.id === active;
                return (
                  <div key={s.id} className={cn("group flex items-center rounded-lg", current ? "bg-secondary text-foreground" : "text-secondary-foreground hover:bg-accent/60")}>
                    <button
                      type="button"
                      onClick={() => onSelect(s.id)}
                      aria-current={current ? "true" : undefined}
                      className="flex min-w-0 flex-1 items-center gap-2.5 rounded-lg p-2 text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                    >
                      <span className={cn("flex h-7 w-7 shrink-0 items-center justify-center rounded-[7px] border border-border text-muted-foreground", s.launcher && "border-dashed", current && "text-foreground")}>
                        {s.launcher ? <Plus className="h-3.5 w-3.5" /> : <Icon className="h-3.5 w-3.5" />}
                      </span>
                      <span className="flex min-w-0 flex-col gap-px">
                        <span className="truncate text-[13px]">{s.title}</span>
                        <span className="truncate text-[11.5px] text-muted-foreground">{subtitle(s, now)}</span>
                      </span>
                    </button>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="mr-1 h-7 w-7 shrink-0 text-muted-foreground opacity-0 focus-visible:opacity-100 group-hover:opacity-100"
                      title="Delete session"
                      aria-label={`Delete session: ${s.title}`}
                      onClick={() => onRemove(s.id)}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                  </div>
                );
              })}
            </div>
          </div>
        ))}
      </nav>
      <div className="hidden items-center justify-between border-t border-border px-4 py-3 text-xs text-muted-foreground md:flex">
        <span>Saved in this browser</span>
        <input ref={picker} type="file" accept="application/json,.json" className="hidden" onChange={(e) => { const f = e.target.files?.[0]; if (f) onImport(f); e.target.value = ""; }} />
        <button type="button" onClick={() => picker.current?.click()} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">Import</button>
      </div>
    </aside>
  );
}
