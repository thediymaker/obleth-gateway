"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { useQuery } from "@tanstack/react-query";
import { ChevronDown, ChevronUp, Copy, X } from "lucide-react";
import { RouteExplainPanel } from "@/components/playground/route-explain";
import { Button } from "@/components/ui/button";
import { modelHref } from "@/lib/model-links";
import { cost, describeRequest, duration, HTTP_REASONS, isFailure, timeSplit, type LogFilters } from "@/lib/logs-model";
import type { SpanEntry, UsageLogEntry } from "@/lib/obleth";
import { parseAttrs, parseRouteExplain, spanHint, spanLabel, timeline, type SpanNode } from "@/lib/trace-model";
import { cn } from "@/lib/utils";

function stamp(ms: number) {
  const d = new Date(ms);
  return `${d.toLocaleDateString([], { month: "short", day: "numeric" })}, ${d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}.${String(d.getMilliseconds()).padStart(3, "0")}`;
}

function title(row: UsageLogEntry): string {
  if (isFailure(row)) return HTTP_REASONS[row.status_code] ?? `Failed with HTTP ${row.status_code}`;
  if (row.cache_status === "hit") return "Answered from the cache";
  return `Answered in ${duration(row.total_ms)}`;
}

function Fact({ label, children, mono }: { label: string; children: React.ReactNode; mono?: boolean }) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5 border-t border-border py-2.5">
      <dt className="text-[11.5px] text-muted-foreground">{label}</dt>
      <dd className={cn("truncate text-[13px]", mono && "font-mono text-[12.5px]")}>{children}</dd>
    </div>
  );
}

/**
 * One step's attributes. The auto router's step shows why it picked the model;
 * a tool loop shows each round's tool and model time.
 */
export function StepDetail({ node, onClose }: { node: SpanNode; onClose: () => void }) {
  const { span, children } = node;
  const attrs = parseAttrs(span.attributes);
  const explain = span.span_name === "auto_route" ? parseRouteExplain(span.attributes) : null;
  const rounds = children
    .filter((c) => c.span.span_name.startsWith("boon:tool_loop:iter:"))
    .sort((a, b) => a.span.start_ms - b.span.start_ms);
  const longest = Math.max(...rounds.map((c) => c.span.duration_ms), 1);
  return (
    <section aria-label={`${spanLabel(span.span_name)} details`} className="rounded-xl border border-border bg-background/40 p-4">
      <div className="mb-2.5 flex items-start justify-between gap-3">
        <div>
          <p className="text-[13px] font-semibold">{spanLabel(span.span_name)}</p>
          {spanHint(span.span_name) && <p className="text-xs text-muted-foreground">{spanHint(span.span_name)}</p>}
        </div>
        <div className="flex items-center gap-3">
          <span className="font-mono text-xs text-muted-foreground">{duration(span.duration_ms)}{span.status === "error" ? " · error" : ""}</span>
          <button type="button" onClick={onClose} aria-label="Close step details" className="text-muted-foreground hover:text-foreground"><X className="h-3.5 w-3.5" /></button>
        </div>
      </div>
      {explain ? (
        <RouteExplainPanel explain={explain} />
      ) : attrs.length > 0 ? (
        <dl className="grid grid-cols-[150px_minmax(0,1fr)] gap-x-3 gap-y-1.5 text-[12.5px]">
          {attrs.map(([k, v]) => (
            <div key={k} className="contents">
              <dt className="text-muted-foreground">{k}</dt>
              <dd className="break-all font-mono">{v}</dd>
            </div>
          ))}
        </dl>
      ) : (
        <p className="text-xs text-muted-foreground">No attributes recorded.</p>
      )}
      {rounds.length > 0 && (
        <div className="mt-3 space-y-1.5">
          <p className="flex items-center gap-3 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">
            Rounds
            <span className="flex items-center gap-1.5 font-normal normal-case tracking-normal"><span className="inline-block h-2 w-3 rounded-sm bg-foreground/80" />tools<span className="ml-1 inline-block h-2 w-3 rounded-sm bg-muted-foreground/60" />model</span>
          </p>
          {rounds.map((c) => {
            const a = parseAttrs(c.span.attributes);
            const raw = Object.fromEntries(a);
            const toolMs = Number(raw.tool_ms ?? 0);
            const modelMs = Number(raw.model_ms ?? 0);
            const both = toolMs + modelMs || 1;
            return (
              <div key={c.span.span_name} className="grid grid-cols-[48px_minmax(0,1fr)_minmax(0,14rem)] items-center gap-2 text-[11.5px]">
                <span className="text-right text-muted-foreground">{spanLabel(c.span.span_name)}</span>
                <span className="relative h-3 overflow-hidden rounded-sm bg-muted/40">
                  <span className="absolute inset-y-0 left-0 flex" style={{ width: `${(c.span.duration_ms / longest) * 100}%` }}>
                    <span className="h-full bg-foreground/80" style={{ width: `${(toolMs / both) * 100}%` }} />
                    <span className="h-full flex-1 bg-muted-foreground/60" />
                  </span>
                </span>
                <span className="truncate font-mono text-muted-foreground" title={String(raw.tools ?? "")}>{duration(toolMs)} + {duration(modelMs)}{raw.tools ? ` · ${raw.tools}` : ""}</span>
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}

export function RequestPanel({
  row,
  onClose,
  onStep,
  canStep,
  onFilter,
}: {
  row: UsageLogEntry;
  onClose: () => void;
  onStep: (dir: -1 | 1) => void;
  canStep: { newer: boolean; older: boolean };
  onFilter: (patch: Partial<LogFilters>) => void;
}) {
  const [selected, setSelected] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const { data: spans, isLoading } = useQuery({
    queryKey: ["spans", row.request_id],
    queryFn: async () => {
      const res = await fetch(`/api/live/usage/logs/${row.request_id}/spans`);
      return res.ok ? ((await res.json()) as SpanEntry[]) : [];
    },
    staleTime: 60_000,
    enabled: row.has_trace,
  });
  useEffect(() => setSelected(null), [row.request_id]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.target instanceof Element && e.target.closest("input, textarea, [contenteditable=true]")) return;
      if (e.key === "Escape") onClose();
      if (e.key === "ArrowUp" || e.key === "k") { e.preventDefault(); onStep(-1); }
      if (e.key === "ArrowDown" || e.key === "j") { e.preventDefault(); onStep(1); }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose, onStep]);

  const failed = isFailure(row);
  const traced = row.has_trace && (spans?.length ?? 0) > 0;
  const tl = traced ? timeline(spans!) : null;
  const split = timeSplit(row);
  const selectedStep = tl?.steps.find((s) => `${s.node.span.span_name}:${s.node.span.start_ms}` === selected)?.node ?? null;
  const copy = () => {
    void navigator.clipboard?.writeText(row.request_id).then(() => { setCopied(true); setTimeout(() => setCopied(false), 1500); });
  };

  return (
    <aside role="dialog" aria-label={`Request ${row.request_id.slice(0, 8)}`} className="fixed inset-y-0 right-0 z-50 flex w-[min(700px,100vw)] flex-col border-l border-border bg-card shadow-2xl">
      <div className="flex flex-col gap-2.5 border-b border-border px-6 pb-4 pt-5">
        <div className="flex items-start justify-between gap-3">
          <div className="flex min-w-0 flex-col gap-1.5">
            <div className="flex items-center gap-2">
              <span className={cn("inline-flex h-[22px] items-center rounded-full border px-2 font-mono text-[11.5px] font-semibold", failed ? "border-foreground bg-foreground text-background" : "border-border text-secondary-foreground")}>{row.status_code}</span>
              <h2 className="truncate text-lg font-semibold">{title(row)}</h2>
            </div>
            <p className="truncate text-[12.5px] text-muted-foreground">
              {stamp(row.ts_ms)} · <span className="font-mono">{row.model}</span> · {row.tenant_name || row.tenant_id.slice(0, 8)}
              {row.key_name && <> · {row.key_name} <span className="font-mono">{row.key_prefix}</span></>}
            </p>
          </div>
          <div className="flex shrink-0 gap-1">
            <Button type="button" variant="outline" size="icon" className="h-8 w-8" disabled={!canStep.newer} onClick={() => onStep(-1)} aria-label="Newer request"><ChevronUp className="h-4 w-4" /></Button>
            <Button type="button" variant="outline" size="icon" className="h-8 w-8" disabled={!canStep.older} onClick={() => onStep(1)} aria-label="Older request"><ChevronDown className="h-4 w-4" /></Button>
            <Button type="button" variant="outline" size="icon" className="h-8 w-8" onClick={onClose} aria-label="Close"><X className="h-4 w-4" /></Button>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button type="button" variant="outline" size="sm" onClick={copy}>
            <Copy className="h-3.5 w-3.5" />
            {copied ? "Copied" : <>Copy ID <span className="font-mono text-muted-foreground">{row.request_id.slice(0, 8)}…</span></>}
          </Button>
          {row.session_id && <Button type="button" variant="outline" size="sm" onClick={() => onFilter({ sessionId: row.session_id })}>This session</Button>}
          {row.key_id && <Button type="button" variant="outline" size="sm" onClick={() => onFilter({ keyId: row.key_id, tenantId: row.tenant_id })}>More from this key</Button>}
          <Button asChild variant="outline" size="sm"><Link href={modelHref(row.model)}>Open {row.model}</Link></Button>
        </div>
      </div>

      <div className="min-h-0 flex-1 space-y-5 overflow-y-auto px-6 py-5">
        <section aria-label="What happened" className="space-y-1.5">
          <p className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">What happened</p>
          {row.has_trace && isLoading ? (
            <div className="skeleton h-10 rounded-lg" />
          ) : (
            <p className="text-[13px] leading-relaxed text-secondary-foreground">{describeRequest(row, spans ?? [])}</p>
          )}
        </section>

        <section aria-label="Timeline" className="space-y-2">
          <div className="flex items-baseline justify-between">
            <p className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Timeline</p>
            <p className="text-[11.5px] text-muted-foreground">one scale · {duration(tl?.totalMs ?? row.total_ms)} in all{traced ? " · click a step for its details" : row.has_trace ? "" : " · not traced"}</p>
          </div>
          {tl ? (
            <div className="space-y-1">
              {tl.steps.map((s) => {
                const id = `${s.node.span.span_name}:${s.node.span.start_ms}`;
                const err = s.node.span.status === "error";
                return (
                  <button
                    key={id}
                    type="button"
                    onClick={() => setSelected(selected === id ? null : id)}
                    aria-pressed={selected === id}
                    className={cn("grid w-full grid-cols-[160px_minmax(0,1fr)_64px] items-center gap-3 rounded-md px-1.5 py-1 text-left text-[12.5px] hover:bg-muted/40", selected === id && "bg-secondary")}
                  >
                    <span className={cn("truncate", err && "font-semibold")} style={{ paddingLeft: s.depth * 12 }}>{spanLabel(s.node.span.span_name)}</span>
                    <span className="relative h-2.5 rounded-[3px] bg-muted/40">
                      <span className={cn("absolute inset-y-0 rounded-[3px]", err ? "bg-foreground" : "bg-muted-foreground")} style={{ left: `${s.left}%`, width: `${s.width}%` }} />
                    </span>
                    <span className="text-right font-mono text-xs text-muted-foreground">{duration(s.node.span.duration_ms)}</span>
                  </button>
                );
              })}
            </div>
          ) : (
            <div className="space-y-1">
              {[
                ["Waiting for a slot", 0, split.wait],
                ["Until the first token", split.wait, split.first],
                ["The rest of the reply", split.wait + split.first, split.rest],
              ].map(([label, from, ms]) => (
                <div key={label as string} className="grid grid-cols-[160px_minmax(0,1fr)_64px] items-center gap-3 px-1.5 py-1 text-[12.5px]">
                  <span>{label}</span>
                  <span className="relative h-2.5 rounded-[3px] bg-muted/40">
                    <span className={cn("absolute inset-y-0 rounded-[3px]", failed && label === "Until the first token" ? "bg-foreground" : "bg-muted-foreground")} style={{ left: `${split.total ? ((from as number) / split.total) * 100 : 0}%`, width: `${split.total ? Math.max(((ms as number) / split.total) * 100, (ms as number) > 0 ? 0.6 : 0) : 0}%` }} />
                  </span>
                  <span className="text-right font-mono text-xs text-muted-foreground">{duration(ms as number)}</span>
                </div>
              ))}
            </div>
          )}
        </section>

        {selectedStep && <StepDetail node={selectedStep} onClose={() => setSelected(null)} />}

        <dl aria-label="Numbers" className="grid grid-cols-2 gap-x-5 sm:grid-cols-4">
          <Fact label="Tokens in" mono>{row.input_tokens.toLocaleString()}</Fact>
          <Fact label="Tokens out" mono>{row.output_tokens.toLocaleString()}</Fact>
          {row.cached_input_tokens != null && <Fact label="Cached input tokens" mono>{row.cached_input_tokens.toLocaleString()}</Fact>}
          <Fact label="Cost" mono>{cost(row.cost_usd)}</Fact>
          <Fact label="Energy" mono>{row.energy_wh > 0 ? `${row.energy_wh.toFixed(2)} Wh · ${row.co2_g.toFixed(2)} g CO₂` : "—"}</Fact>
          <Fact label="Waited for a slot" mono>{duration(row.queue_wait_ms)}</Fact>
          <Fact label="First token" mono>{row.ttft_ms > 0 ? duration(row.ttft_ms) : "—"}</Fact>
          <Fact label="Total" mono>{duration(row.total_ms)}</Fact>
          <Fact label="Type">{row.request_type || "other"}</Fact>
          <Fact label="Session" mono>{row.session_id ? <span title={row.session_id}>{row.session_id}</span> : "—"}</Fact>
          <Fact label="Session came from">{row.session_id_source === "client" ? "the client" : row.session_id_source === "derived" ? "derived by the gateway" : "—"}</Fact>
          <Fact label="Cache">{row.cache_status || "—"}</Fact>
          <Fact label="Admission">{row.admission || "—"}</Fact>
          {row.device_id && <Fact label="Device" mono>{row.device_id}</Fact>}
          <Fact label="Request ID" mono><span title={row.request_id}>{row.request_id}</span></Fact>
        </dl>
      </div>
    </aside>
  );
}
