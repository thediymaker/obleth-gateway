"use client";

import { memo, useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { useFairshareHistory, usePoolLoad } from "@/components/fairshare/hooks";
import { Panel } from "@/components/overview/ui";
import type { AuditEntry, ModelRoute, UsageLogHistogram } from "@/lib/obleth";
import { getJson } from "@/lib/utils";
import { clock, dateTime } from "@/lib/format";
import { Tile } from "@/components/ui/tile";

const DAY_MS = 86_400_000;

/** Requests and failures for one model in the last 24 hours, counted like the request log. */
export function useModelDay(model: string) {
  return useQuery({
    queryKey: ["deployment-day", model],
    queryFn: async () => {
      const since = Date.now() - DAY_MS;
      const h = await getJson<UsageLogHistogram>(`/api/live/usage/logs/histogram?model=${encodeURIComponent(model)}&since_ms=${since}&bucket_ms=3600000`);
      return {
        requests: h.buckets.reduce((n, b) => n + Number(b.requests), 0),
        errors: h.buckets.reduce((n, b) => n + Number(b.errors), 0),
      };
    },
    refetchInterval: 60_000,
  });
}

/** The "Pool now" tile, reading the live load itself so a poll re-renders only it. */
export function PoolNowTile({ model }: { model: ModelRoute }) {
  const { load: { inFlight, cap, queued } } = usePoolLoad(model);
  return (
    <Tile
      label="Pool now"
      value={<>{inFlight} <span className="text-[13px] font-normal text-muted-foreground">{cap ? `of ${cap}` : "in flight"}</span></>}
      meter={[inFlight, cap]}
      detail={queued ? `${queued} waiting` : "none waiting"}
    />
  );
}

/** The last hour's load against the pool's size now. */
export function PoolChart({ model }: { model: ModelRoute }) {
  return <InFlightChart model={model.model_name} cap={usePoolLoad(model).load.cap} />;
}

/** In flight and waiting over the last hour, from the scheduler's own samples of this model's pool. */
export const InFlightChart = memo(function InFlightChart({ model, cap }: { model: string; cap: number }) {
  const history = useFairshareHistory(model);
  const bars = useMemo(() => {
    const pts = history.history ?? [];
    if (pts.length === 0) return [];
    // Each sample holds the slots running per group; their sum is the pool's in flight.
    const running = (p: (typeof pts)[number]) => history.groupKeys.reduce((n, g) => n + Number(p[g.key] ?? 0), 0);
    const n = 60;
    const step = Math.max(1, Math.ceil(pts.length / n));
    const out: { inFlight: number; queued: number; ts: number }[] = [];
    for (let i = 0; i < pts.length; i += step) {
      const slice = pts.slice(i, i + step);
      out.push({ inFlight: Math.max(...slice.map(running)), queued: Math.max(...slice.map((p) => Number(p.queued))), ts: slice[slice.length - 1].ts });
    }
    return out;
  }, [history.history, history.groupKeys]);
  const max = Math.max(1, cap, ...bars.map((b) => b.inFlight + b.queued));
  const peak = bars.reduce((m, b) => Math.max(m, b.inFlight), 0);
  const waited = bars.some((b) => b.queued > 0);
  return (
    <Panel title="Load · last hour" subtitle={`Requests in flight against the pool of ${cap || "no fixed"} slots${waited ? " · bright: waiting for a slot" : ""}`}>
      <div className="px-[18px] pb-3 pt-2">
        {bars.length === 0 ? (
          <p className="py-10 text-center text-[12.5px] text-muted-foreground">No samples yet. The gateway records its pools every 2 seconds.</p>
        ) : (
          <>
            <div className="relative flex h-[150px] items-end gap-[2px] border-b border-border" aria-label={`Peak ${peak} in flight`}>
              {cap > 0 && <span className="absolute inset-x-0 border-t border-dashed border-muted-foreground/60" style={{ bottom: `${(cap / max) * 100}%` }} aria-hidden />}
              {bars.map((b, i) => (
                <span key={i} className="flex h-full flex-1 flex-col justify-end" title={`${clock(b.ts)}: ${b.inFlight} in flight${b.queued ? `, ${b.queued} waiting` : ""}`}>
                  {b.queued > 0 && <span className="block rounded-t-[2px] bg-foreground" style={{ height: `${(b.queued / max) * 100}%` }} />}
                  <span className="block bg-muted-foreground/70" style={{ height: `${(b.inFlight / max) * 100}%` }} />
                </span>
              ))}
            </div>
            <div className="flex justify-between pt-1.5 font-mono text-[10.5px] text-muted-foreground"><span>an hour ago</span><span>peak {peak}{cap ? ` of ${cap}` : ""}</span><span>now</span></div>
          </>
        )}
      </div>
    </Panel>
  );
});

const ACTION_WORDS: Record<string, string> = {
  put_managed_model: "Launch settings saved",
  delete_managed_model: "Stopped launching it",
  clear_lost_replicas: "Cleared failed launches",
  clear_provision_error: "Submit error cleared",
  retry_managed_model: "Asked it to try launching again",
  roll_managed_model: "Replacing running replicas one at a time",
  restart_replica: "Replica restarted",
  create_model: "Model added",
  update_model: "Model settings saved",
  set_model_capacity: "Capacity changed",
  set_model_capacity_mode: "Capacity mode changed",
  set_model_reliability: "Retries and timeouts changed",
  set_model_cache: "Cache changed",
};

/** Recent changes to the deployment, from the audit log. */
export function ChangesList({ changes }: { changes: AuditEntry[] }) {
  return (
    <Panel title="Changes" subtitle="From the audit log" label="Changes">
      <div id="changes" className="scroll-mt-24 px-[18px] pb-3 pt-2">
        {changes.length === 0 ? (
          <p className="py-2 text-[12.5px] text-muted-foreground">No changes recorded.</p>
        ) : (
          changes.map((e) => {
            const d = (e.detail ?? {}) as Record<string, unknown>;
            const detail = e.action === "put_managed_model" ? `${d.target_replicas ?? "?"} replica${d.target_replicas === 1 ? "" : "s"}${d.partition ? ` on ${d.partition}` : ""}${d.enabled === false ? ", paused" : ""}` : "";
            return (
              <div key={e.id} className="grid grid-cols-[140px_minmax(0,1fr)_minmax(0,220px)] gap-3 border-t border-border py-2 text-[12.5px] first:border-t-0">
                <span className="font-mono text-[11.5px] text-muted-foreground">{dateTime(e.ts)}</span>
                <span className="truncate">{ACTION_WORDS[e.action] ?? e.action.replace(/_/g, " ")}{detail && <span className="text-muted-foreground"> · {detail}</span>}</span>
                <span className="truncate text-right text-muted-foreground">{e.actor}</span>
              </div>
            );
          })
        )}
      </div>
    </Panel>
  );
}
