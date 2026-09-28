"use client";

import { useRef, useState, useTransition } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, Trash2 } from "lucide-react";
import { createModelEndpointAction, deleteModelEndpointAction, restartReplicaAction, updateModelEndpointAction, type ActionResult } from "@/app/actions";
import { Field } from "@/components/models/fields";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import type { ModelEndpoint, ModelReplica, ModelRoute } from "@/lib/obleth";
import { cn } from "@/lib/utils";

/**
 * The model's extra upstreams. Unlike the settings above it, every change
 * here applies at once: an endpoint is a row of its own, not a field of the
 * model.
 */
export function EndpointsSection({ model, endpoints, onChanged }: { model: ModelRoute; endpoints: ModelEndpoint[]; onChanged: () => void }) {
  const [busy, start] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const addForm = useRef<HTMLFormElement>(null);
  const { confirm, confirmElement } = useConfirm();
  const queryClient = useQueryClient();

  // A Slurm-backed endpoint maps to a replica, which can be restarted: its job
  // is cancelled and the provisioner launches a fresh one.
  const { data: replicas } = useQuery({
    queryKey: ["replicas", model.id],
    refetchInterval: 5000,
    queryFn: async (): Promise<ModelReplica[]> => {
      const r = await fetch(`/api/live/models/${model.id}/replicas`);
      if (!r.ok) throw new Error("failed to load replicas");
      return r.json();
    },
  });
  const replicaByEndpoint = new Map((replicas ?? []).filter((r) => r.endpoint_id).map((r) => [r.endpoint_id as string, r]));

  const run = (task: () => Promise<ActionResult>) =>
    start(async () => {
      setError(null);
      const result = await task();
      if (!result.ok) setError(result.error);
      await queryClient.invalidateQueries({ queryKey: ["model-endpoints", model.id] });
      onChanged();
    });

  const add = (data: FormData) =>
    run(async () => {
      const result = await createModelEndpointAction(model.id, data);
      if (result.ok) {
        addForm.current?.reset();
        setAdding(false);
      }
      return result;
    });

  return (
    <section id="endpoints" data-section="endpoints" aria-label="Endpoints" className="scroll-mt-24 rounded-xl border border-border bg-card">
      {confirmElement}
      <header className="flex flex-wrap items-start justify-between gap-3 px-[18px] pb-3 pt-4">
        <div>
          <h2 className="text-sm font-semibold">Endpoints</h2>
          <p className="mt-0.5 text-xs text-muted-foreground">
            {endpoints.length ? "More upstreams serving this model. Changes here apply at once." : "Requests go to the API base above. Add an endpoint to spread them over more upstreams."}
          </p>
        </div>
        {!adding && <Button type="button" size="sm" variant="outline" onClick={() => setAdding(true)}><Plus className="h-3.5 w-3.5" />Add endpoint</Button>}
      </header>
      {error && <p role="alert" className="mx-[18px] mb-3 rounded-lg border border-foreground/60 px-3 py-2 text-[12.5px]">{error}</p>}
      {endpoints.length > 0 && (
        <div className="overflow-x-auto border-t border-border">
          <table className="w-full min-w-[720px] text-left text-[13px]">
            <thead>
              <tr className="border-b border-border text-[10.5px] uppercase tracking-[0.07em] text-muted-foreground">
                <th className="py-2 pl-[18px] pr-3 font-semibold">Name</th>
                <th className="py-2 pr-3 font-semibold">API base</th>
                <th className="py-2 pr-3 text-right font-semibold">Priority</th>
                <th className="py-2 pr-3 text-right font-semibold">Weight</th>
                <th className="py-2 pr-3 text-right font-semibold" title="Requests this endpoint takes at once (discovered capacity)">Max in flight</th>
                <th className="py-2 pr-3 font-semibold">Health</th>
                <th className="py-2 pr-[18px]" />
              </tr>
            </thead>
            <tbody>
              {endpoints.map((ep) => (
                <tr key={ep.id} className={cn("border-b border-border/60 last:border-b-0", !ep.enabled && "text-muted-foreground")}>
                  <td className="py-2 pl-[18px] pr-3 font-medium">
                    {ep.name}
                    {ep.last_message && ep.health_status !== "healthy" && <span className="block max-w-[32ch] truncate text-[11px] font-normal text-muted-foreground" title={ep.last_message}>{ep.last_message}</span>}
                  </td>
                  <td className="max-w-[20rem] truncate py-2 pr-3 font-mono text-[11.5px] text-muted-foreground" title={ep.api_base}>{ep.api_base}</td>
                  <td className="py-2 pr-3 text-right tabular-nums">{ep.priority}</td>
                  <td className="py-2 pr-3 text-right tabular-nums">{ep.weight}</td>
                  <td className="py-2 pr-3 text-right tabular-nums">{ep.max_in_flight ?? "—"}</td>
                  <td className="py-2 pr-3">
                    {!ep.enabled ? "off" : ep.health_status === "unhealthy" ? <span className="rounded-full bg-foreground px-1.5 text-[10.5px] font-bold text-background">DOWN</span> : ep.health_status}
                  </td>
                  <td className="py-2 pr-[18px]">
                    <div className="flex items-center justify-end gap-1">
                      {replicaByEndpoint.has(ep.id) && (
                        <Button type="button" size="sm" variant="ghost" disabled={busy} title="Cancel this replica's Slurm job; the provisioner launches a fresh one" onClick={() => run(() => restartReplicaAction(replicaByEndpoint.get(ep.id)!.id))}>
                          Restart
                        </Button>
                      )}
                      <Button
                        type="button"
                        size="sm"
                        variant="ghost"
                        disabled={busy}
                        onClick={() => run(() => updateModelEndpointAction(model.id, ep.id, { name: ep.name, api_base: ep.api_base, priority: ep.priority, weight: ep.weight, enabled: !ep.enabled }))}
                      >
                        {ep.enabled ? "Turn off" : "Turn on"}
                      </Button>
                      <Button
                        type="button"
                        size="sm"
                        variant="ghost"
                        disabled={busy}
                        aria-label={`Remove ${ep.name}`}
                        onClick={async () => {
                          const ok = await confirm({ title: "Remove endpoint", description: `Remove endpoint "${ep.name}"? Traffic shifts to the model's other endpoints.`, confirmLabel: "Remove" });
                          if (ok) run(() => deleteModelEndpointAction(model.id, ep.id));
                        }}
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </Button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {adding && (
        <form ref={addForm} action={add} className="border-t border-border px-[18px] py-4">
          <div className="grid gap-3 md:grid-cols-2">
            <Field label="Name" name="name" placeholder="cluster-b" required />
            <Field label="API base URL" name="api_base" placeholder="http://cluster-b/v1" required className="[&_input]:font-mono" />
            <Field label="API key" name="api_key" type="password" placeholder="Blank uses the model's key" autoComplete="new-password" />
            <div className="grid grid-cols-3 gap-3">
              <Field label="Priority" name="priority" type="number" defaultValue="100" />
              <Field label="Weight" name="weight" type="number" defaultValue="100" />
              <Field label="Max in flight" name="max_in_flight" type="number" min={1} placeholder="model's" />
            </div>
          </div>
          <div className="mt-3 flex gap-2">
            <Button type="submit" size="sm" disabled={busy}>Add endpoint</Button>
            <Button type="button" size="sm" variant="ghost" disabled={busy} onClick={() => setAdding(false)}>Cancel</Button>
          </div>
        </form>
      )}
    </section>
  );
}
