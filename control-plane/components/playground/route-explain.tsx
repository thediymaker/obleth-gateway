"use client";

import { useState } from "react";
import { AlertTriangle, ChevronDown, ChevronRight } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";
import type { RouteExplainView } from "@/lib/obleth";

const fmt = (n: number) => n.toFixed(3);

/**
 * Renders one `auto` routing decision: the stage strip (difficulty, tags,
 * tier), the ranked candidates with their arithmetic, the collapsed
 * rejections, and the tier-floor-clamp warning.
 *
 * Shared by the Playground's Router mode (Task 13, where `baseline` carries
 * what the *live/saved* weights would have picked, so an operator can see
 * both columns for one edit) and the trace viewer (Task 14, which explains a
 * single real decision and never passes `baseline`).
 */
export function RouteExplainPanel({
  explain,
  baseline,
}: {
  explain: RouteExplainView;
  baseline?: RouteExplainView;
}) {
  const [expanded, setExpanded] = useState<string | null>(null);
  const decisionChanged = !!baseline && baseline.chosen !== explain.chosen;
  const gridCols = baseline ? "grid-cols-[minmax(0,1fr)_5rem_5rem]" : "grid-cols-[minmax(0,1fr)_5rem]";

  return (
    <div className="space-y-4">
      {explain.tier_floor_clamped && (
        <p
          role="status"
          className="flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm text-amber-700 dark:text-amber-400"
        >
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" aria-hidden="true" />
          <span>
            The strongest model for this topic was unavailable, so the request was clamped down
            to tier {explain.tier_floor}.
          </span>
        </p>
      )}

      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted-foreground">
        <span>
          Difficulty <span className="font-medium text-foreground">{explain.difficulty}</span> ({explain.difficulty_source})
        </span>
        <span>
          Tags{" "}
          <span className="font-medium text-foreground">
            {explain.tags.length ? explain.tags.join(", ") : "none"}
          </span>{" "}
          ({explain.tag_source})
        </span>
        {explain.tier_domains.length > 0 && (
          <span>
            Domains <span className="font-medium text-foreground">{explain.tier_domains.join(", ")}</span>
          </span>
        )}
        <span>
          Temperature <span className="font-medium text-foreground">{explain.temperature.toFixed(2)}</span>
        </span>
        {explain.temperature > 0 && (
          <span>
            Draw <span className="font-medium text-foreground">{explain.uniform.toFixed(3)}</span>
            {explain.sampled ? " (sampled)" : " (top scorer still won)"}
          </span>
        )}
        {explain.classifier_ms > 0 && <span>Classifier {explain.classifier_ms}ms</span>}
      </div>

      {baseline && (
        <p className="text-sm">
          {decisionChanged ? (
            <>
              <strong>{baseline.chosen ?? "No candidate"}</strong> was chosen by the live weights;
              the edited weights choose <strong>{explain.chosen ?? "no candidate"}</strong> instead.
            </>
          ) : (
            <>
              Live and edited weights both choose <strong>{explain.chosen ?? "no candidate"}</strong>.
            </>
          )}
        </p>
      )}

      <div className="overflow-hidden rounded-lg border border-border">
        <div
          className={cn(
            "grid gap-2 border-b border-border bg-secondary/30 px-3 py-2 text-xs font-medium text-muted-foreground",
            gridCols,
          )}
        >
          <span>Model</span>
          {baseline && <span className="text-right">Live</span>}
          <span className="text-right">{baseline ? "Edited" : "Score"}</span>
        </div>
        <div className="divide-y divide-border">
          {explain.scored.map((row) => {
            const baselineRow = baseline?.scored.find((c) => c.model === row.model);
            const isExpanded = expanded === row.model;
            const wasLiveChoice = decisionChanged && baseline?.chosen === row.model;
            return (
              <div key={row.model}>
                <button
                  type="button"
                  aria-expanded={isExpanded}
                  onClick={() => setExpanded(isExpanded ? null : row.model)}
                  className={cn(
                    "grid w-full items-center gap-2 px-3 py-2 text-left text-sm hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                    gridCols,
                  )}
                >
                  <span className="flex min-w-0 items-center gap-2">
                    {isExpanded ? (
                      <ChevronDown className="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
                    ) : (
                      <ChevronRight className="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
                    )}
                    <span className="truncate">{row.model}</span>
                    {row.chosen && <Badge className="shrink-0 border-violet-500/30 text-violet-600 dark:text-violet-400">chosen</Badge>}
                    {wasLiveChoice && (
                      <Badge className="shrink-0 border-amber-500/30 text-amber-600 dark:text-amber-400">live pick</Badge>
                    )}
                  </span>
                  {baseline && (
                    <span className="text-right tabular-nums text-muted-foreground">
                      {baselineRow ? fmt(baselineRow.score) : "—"}
                    </span>
                  )}
                  <span className="text-right tabular-nums">{fmt(row.score)}</span>
                </button>
                {isExpanded && (() => {
                  // Mirrors the gateway's scoring exactly (obleth-config's
                  // routing/mod.rs): capacity/cost form a base, tags (when
                  // requested) blend into it, and `bias` is a multiplier on
                  // top of that blend — not a fourth addend. `row.score` is
                  // always the value the API actually returned; `base` and
                  // `blend` here are recomputed from the same fields purely
                  // to narrate how it was reached.
                  const { capacity, cost, tag } = explain.weights;
                  const hasTags = explain.tags.length > 0;
                  const base = capacity * row.spare + cost * row.cost_score;
                  const blend = hasTags ? tag * row.tag_score + (1 - tag) * base : base;
                  return (
                    <div className="space-y-1.5 bg-secondary/10 px-3 py-3 text-xs">
                      <p>
                        Spare capacity: <span className="tabular-nums">{fmt(row.spare)}</span> · Cost
                        score: <span className="tabular-nums">{fmt(row.cost_score)}</span> · Tag
                        score: <span className="tabular-nums">{fmt(row.tag_score)}</span> · Bias:{" "}
                        <span className="tabular-nums">{fmt(row.bias)}</span>
                      </p>
                      <p className="font-mono text-muted-foreground">
                        base = {fmt(capacity)} × {fmt(row.spare)} (spare) + {fmt(cost)} × {fmt(row.cost_score)} (cost)
                        {" "}= <strong className="text-foreground">{fmt(base)}</strong>
                      </p>
                      {hasTags ? (
                        <p className="font-mono text-muted-foreground">
                          blend = {fmt(tag)} × {fmt(row.tag_score)} (tag) + {fmt(1 - tag)} × {fmt(base)} (base) ={" "}
                          <strong className="text-foreground">{fmt(blend)}</strong>
                        </p>
                      ) : (
                        <p className="font-mono text-muted-foreground">
                          No requested tags, so blend = base = <strong className="text-foreground">{fmt(base)}</strong>
                        </p>
                      )}
                      <p className="font-mono">
                        score = {fmt(blend)} (blend) × {fmt(row.bias)} (bias) = <strong>{fmt(row.score)}</strong>
                      </p>
                      {wasLiveChoice && (
                        <p className="text-amber-600 dark:text-amber-400">
                          This model was chosen by the live weights.
                        </p>
                      )}
                    </div>
                  );
                })()}
              </div>
            );
          })}
          {explain.scored.length === 0 && (
            <p className="px-3 py-3 text-xs text-muted-foreground">No candidates scored.</p>
          )}
        </div>
      </div>

      {explain.rejected.length > 0 && (
        <div className="space-y-2">
          <p className="text-xs font-medium text-muted-foreground">Not considered</p>
          {explain.rejected.map((r) => (
            <div key={r.reason} className="rounded-md border border-border bg-secondary/10 px-3 py-2 text-xs">
              <p className="font-medium">{r.reason}</p>
              <p className="text-muted-foreground">{r.models.join(", ")}</p>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
