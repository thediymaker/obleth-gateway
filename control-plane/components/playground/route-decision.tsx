"use client";

import { useState } from "react";
import { AlertTriangle, ArrowRight, ChevronDown, ChevronRight } from "lucide-react";
import { Button } from "@/components/ui/button";
import type { RouteExplainView, ScoredCandidateView } from "@/lib/obleth";
import { cn } from "@/lib/utils";
import { Pill } from "./ui";

const fmt = (n: number) => n.toFixed(3);
const fmt2 = (n: number) => n.toFixed(2);

/** The synthetic domain covering every chat candidate travels as `"*"`; spell it out. */
const domainList = (domains: string[]) =>
  domains.length ? domains.map((d) => (d === "*" ? "all models" : d)).join(", ") : "all models";

const SOURCE: Record<string, string> = { classifier: "Live classifier", heuristic: "Keyword heuristic", header: "Request header", default: "Default" };

/**
 * Mirrors the gateway's scoring (obleth-config's routing/mod.rs): capacity and
 * cost form a base, tags (when requested) blend into it, and `bias` multiplies
 * the blend. `row.score` is always the value the API returned; the parts here
 * are recomputed from the same fields only to draw and narrate it.
 */
function parts(row: ScoredCandidateView, explain: RouteExplainView) {
  const { capacity, cost, tag } = explain.weights;
  const hasTags = explain.tags.length > 0;
  const base = capacity * row.spare + cost * row.cost_score;
  const blend = hasTags ? tag * row.tag_score + (1 - tag) * base : base;
  const keep = hasTags ? 1 - tag : 1;
  return {
    base, blend, hasTags,
    segments: {
      capacity: keep * capacity * row.spare * row.bias,
      cost: keep * cost * row.cost_score * row.bias,
      tag: hasTags ? tag * row.tag_score * row.bias : 0,
    },
  };
}

const SEGMENT_COLORS = { capacity: "bg-foreground", cost: "bg-muted-foreground", tag: "bg-muted-foreground/40" } as const;

/**
 * The Router mode's reading of one `auto` decision: what it picked, the four
 * stages that got there, and every candidate's score broken into the parts
 * the weights control. With `baseline`, draft and live scores sit side by side.
 */
export function RouteDecision({ explain, baseline, onOpenInChat }: {
  explain: RouteExplainView;
  /** What the live weights decide for the same request; pass only while the draft differs. */
  baseline?: RouteExplainView;
  onOpenInChat?: (model: string) => void;
}) {
  const [expanded, setExpanded] = useState<string | null>(explain.chosen);
  const changed = !!baseline && baseline.chosen !== explain.chosen;
  const tiering = explain.weights.difficulty_enabled;
  const chosenRow = explain.scored.find((r) => r.chosen);
  const rejectedCount = explain.rejected.reduce((n, r) => n + r.models.length, 0);
  const total = explain.scored.length + rejectedCount;
  const scale = Math.max(1, ...explain.scored.map((r) => r.score));
  const cols = baseline ? "grid-cols-[minmax(0,1fr)_minmax(0,14rem)_3.5rem_3.5rem]" : "grid-cols-[minmax(0,1fr)_minmax(0,14rem)_3.5rem]";

  return (
    <div className="space-y-4">
      {explain.tier_floor_clamped && (
        <p role="status" className="flex items-start gap-2 rounded-xl border border-foreground/40 px-3.5 py-2.5 text-sm">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" aria-hidden="true" />
          <span>The strongest model for this topic was unavailable, so the request was clamped down to tier {explain.tier_floor}.</span>
        </p>
      )}

      <div className="flex flex-wrap items-center gap-3.5 rounded-2xl border border-border bg-card/60 px-4 py-3.5">
        <div className="min-w-0 flex-1 space-y-1">
          <div className="text-xs text-muted-foreground">{baseline ? "With your draft weights, auto routes this to" : "auto routes this to"}</div>
          <div className="flex flex-wrap items-baseline gap-x-2.5">
            <span className="truncate text-xl font-semibold">{explain.chosen ?? "no candidate"}</span>
            {chosenRow && <span className="font-mono text-[13px] text-muted-foreground">score {fmt(chosenRow.score)}</span>}
          </div>
        </div>
        {baseline && (
          <div className="flex flex-col items-start gap-1.5 sm:items-end">
            {changed ? <Pill inverted>Pick changed</Pill> : <Pill>Same pick as live</Pill>}
            {changed && <span className="text-xs text-muted-foreground">Live weights pick {baseline.chosen ?? "no candidate"}</span>}
          </div>
        )}
        {explain.chosen && onOpenInChat && (
          <Button variant="outline" onClick={() => onOpenInChat(explain.chosen!)}>Open in Chat<ArrowRight className="h-3.5 w-3.5" /></Button>
        )}
      </div>

      <ol aria-label="Routing stages" className="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
        <Stage n={1} title="Classify" caption={`${SOURCE[explain.tag_source] ?? explain.tag_source}${explain.classifier_ms > 0 ? ` · ${explain.classifier_ms} ms` : ""}`}>
          <div className="flex flex-wrap gap-1">
            {explain.tags.length ? explain.tags.map((t) => <Pill key={t}>{t}</Pill>) : <span className="text-[13px] text-muted-foreground">No tags</span>}
            <Pill>difficulty {explain.difficulty}</Pill>
          </div>
        </Stage>
        <Stage n={2} title="Filter" caption={tiering ? `Tier floor ${explain.tier_floor} · ${domainList(explain.tier_domains)}` : "Difficulty tiering off"}>
          <span className="text-[13px]">{explain.scored.length} of {total} eligible</span>
        </Stage>
        <Stage n={3} title="Score" caption={explain.tags.length ? "Capacity · cost · tag" : "Capacity · cost"}>
          <span className="text-[13px]">{explain.scored.length} candidate{explain.scored.length === 1 ? "" : "s"}</span>
        </Stage>
        <Stage n={4} title="Pick" final caption={`Temperature ${fmt2(explain.temperature)}${explain.temperature > 0 ? ` · draw ${fmt(explain.uniform)}${explain.sampled ? ", sampled" : ", top scorer won"}` : ""}`}>
          <span className="truncate text-[13px]">{explain.chosen ?? "—"}</span>
        </Stage>
      </ol>

      <div className="overflow-x-auto rounded-xl border border-border">
        <div className="min-w-[36rem]">
          <div className={cn("grid items-center gap-3 border-b border-border bg-card px-3.5 py-2 text-[11.5px] text-muted-foreground", cols)}>
            <span>Candidate</span>
            <span className="flex flex-wrap gap-x-2.5">
              {(["capacity", "cost", "tag"] as const).filter((k) => k !== "tag" || explain.tags.length > 0).map((k) => (
                <span key={k} className="inline-flex items-center gap-1"><span className={cn("h-2.5 w-2.5 rounded-sm", SEGMENT_COLORS[k])} />{k}</span>
              ))}
            </span>
            {baseline && <span className="text-right">Live</span>}
            <span className={cn("text-right", baseline && "text-foreground")}>{baseline ? "Draft" : "Score"}</span>
          </div>
          {explain.scored.length === 0 && <p className="px-3.5 py-3 text-xs text-muted-foreground">No candidates scored.</p>}
          {explain.scored.map((row) => {
            const p = parts(row, explain);
            const live = baseline?.scored.find((c) => c.model === row.model);
            const open = expanded === row.model;
            const wasLive = changed && baseline?.chosen === row.model;
            return (
              <div key={row.model} className="border-b border-border last:border-b-0">
                <button
                  type="button"
                  aria-expanded={open}
                  onClick={() => setExpanded(open ? null : row.model)}
                  className={cn("grid w-full items-center gap-3 px-3.5 py-2.5 text-left hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring", cols, open && "bg-accent/40")}
                >
                  <span className="flex min-w-0 items-center gap-2">
                    {open ? <ChevronDown className="h-3.5 w-3.5 shrink-0" aria-hidden="true" /> : <ChevronRight className="h-3.5 w-3.5 shrink-0" aria-hidden="true" />}
                    <span className="truncate text-[13px]">{row.model}</span>
                    {row.chosen && <Pill inverted>Chosen</Pill>}
                    {wasLive && <Pill>Live pick</Pill>}
                    {tiering && row.level > 0 && <span className="shrink-0 text-[11.5px] text-muted-foreground">tier {row.level}</span>}
                  </span>
                  <span className="flex h-2.5 overflow-hidden rounded-sm bg-muted/40" aria-hidden="true">
                    {(["capacity", "cost", "tag"] as const).map((k) => (
                      <span key={k} className={SEGMENT_COLORS[k]} style={{ width: `${Math.max(0, (p.segments[k] / scale) * 100)}%` }} />
                    ))}
                  </span>
                  {baseline && <span className="text-right font-mono text-[12.5px] text-muted-foreground">{live ? fmt(live.score) : "—"}</span>}
                  <span className="text-right font-mono text-[12.5px] font-medium">{fmt(row.score)}</span>
                </button>
                {open && (
                  <div className="space-y-1 bg-accent/40 px-3.5 pb-3 pl-9 text-[11.5px] leading-relaxed">
                    {tiering && (
                      <p className="text-muted-foreground">
                        Tier level <span className="text-foreground">{row.level}</span> in {domainList(explain.tier_domains)} · floor <span className="text-foreground">{explain.tier_floor}</span>
                        {row.level >= explain.tier_floor ? " — cleared the floor" : " — below the floor"}
                      </p>
                    )}
                    <p className="text-muted-foreground">
                      Spare capacity {fmt(row.spare)} · cost score {fmt(row.cost_score)} · tag score {fmt(row.tag_score)} · bias {fmt(row.bias)}
                    </p>
                    <p className="font-mono text-muted-foreground">
                      base = {fmt2(explain.weights.capacity)} × {fmt(row.spare)} spare + {fmt2(explain.weights.cost)} × {fmt(row.cost_score)} cost = <span className="text-foreground">{fmt(p.base)}</span>
                    </p>
                    <p className="font-mono text-muted-foreground">
                      {p.hasTags
                        ? <>blend = {fmt2(explain.weights.tag)} × {fmt(row.tag_score)} tag + {fmt2(1 - explain.weights.tag)} × {fmt(p.base)} base = <span className="text-foreground">{fmt(p.blend)}</span></>
                        : <>No requested tags, so blend = base = <span className="text-foreground">{fmt(p.blend)}</span></>}
                    </p>
                    <p className="font-mono">score = {fmt(p.blend)} blend × {fmt(row.bias)} bias = <strong>{fmt(row.score)}</strong></p>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>

      {explain.rejected.length > 0 && (
        <details className="group rounded-xl border border-dashed border-border px-3.5 py-2.5 text-[12.5px]">
          <summary className="flex cursor-pointer list-none items-center gap-2 text-muted-foreground">
            <ChevronRight className="h-3.5 w-3.5 transition-transform group-open:rotate-90" aria-hidden="true" />
            <span className="text-foreground">Not considered ({rejectedCount})</span>
            <span className="truncate">{explain.rejected.map((r) => r.reason).join(" · ")}</span>
          </summary>
          <div className="mt-2.5 space-y-2 pl-5">
            {explain.rejected.map((r) => (
              <div key={r.reason}>
                <p className="font-medium">{r.reason}</p>
                <p className="text-muted-foreground">{r.models.join(", ")}</p>
              </div>
            ))}
          </div>
        </details>
      )}
    </div>
  );
}

function Stage({ n, title, caption, final, children }: { n: number; title: string; caption: string; final?: boolean; children: React.ReactNode }) {
  return (
    <li className={cn("flex min-w-0 flex-col gap-1.5 rounded-xl border bg-card/60 px-3 py-2.5", final ? "border-muted-foreground/60" : "border-border")}>
      <div className="flex items-center gap-2 text-[12.5px] font-semibold">
        <span className={cn("flex h-[18px] w-[18px] items-center justify-center rounded-full border text-[10.5px]", final ? "border-foreground bg-foreground text-background" : "border-border text-secondary-foreground")}>{n}</span>
        {title}
      </div>
      {children}
      <div className="truncate text-[11.5px] text-muted-foreground" title={caption}>{caption}</div>
    </li>
  );
}
