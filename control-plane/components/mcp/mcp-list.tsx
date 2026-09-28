"use client";

import { useCallback, useEffect, useMemo, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import { Plus } from "lucide-react";
import { createMcpServerAction } from "@/app/actions";
import { McpStateMark, ServerSheet } from "@/components/mcp/server-sheet";
import { Field } from "@/components/models/fields";
import { Notice, Sheet, Tile } from "@/components/models/ui";
import { Button } from "@/components/ui/button";
import type { McpServerTestRow } from "@/lib/charo/mcp/types";
import { buildMcpRows, mcpLine, probeLatency, toolCapable } from "@/lib/mcp-model";
import type { DailyStatsView, McpServer, ModelRoute } from "@/lib/obleth";
import { compact } from "@/lib/overview-model";
import { cn, getJson } from "@/lib/utils";

const COLS = "grid-cols-[minmax(0,1.1fr)_150px_70px_minmax(0,1.4fr)_110px_minmax(0,1.2fr)_96px]";

async function probe(name: string): Promise<McpServerTestRow> {
  try {
    const res = await fetch(`/api/mcp/${encodeURIComponent(name)}/probe`, { method: "POST" });
    if (!res.ok) throw new Error(`the check failed (HTTP ${res.status})`);
    return (await res.json()) as McpServerTestRow;
  } catch (e) {
    return { server: name, status: "fail", message: e instanceof Error ? e.message : String(e), serverInfo: null, protocolVersion: null, tools: null, latencyMs: null };
  }
}

/** Every server checked when the page opens, and again on request. */
export function useProbes(servers: McpServer[]) {
  const [probes, setProbes] = useState<Record<string, McpServerTestRow>>({});
  const check = useCallback(async (name: string) => {
    setProbes((p) => { const n = { ...p }; delete n[name]; return n; });
    const row = await probe(name);
    setProbes((p) => ({ ...p, [name]: row }));
  }, []);
  const names = servers.filter((s) => s.enabled).map((s) => s.name).join("|");
  useEffect(() => {
    for (const n of names.split("|").filter(Boolean)) void check(n);
  }, [names, check]);
  return { probes, check, checkAll: () => { for (const n of names.split("|").filter(Boolean)) void check(n); } };
}

function AddSheet({ open, onClose, onAdded }: { open: boolean; onClose: () => void; onAdded: (name: string) => void }) {
  const [pending, start] = useTransition();
  const [error, setError] = useState<string | null>(null);
  return (
    <Sheet
      open={open}
      onClose={onClose}
      title="Add a tool server"
      description="A streamable-HTTP MCP server. obleth checks it and lists its tools; then you choose which models may use them."
      width="w-[min(520px,100vw)]"
      footer={
        <div className="flex items-center justify-between gap-3">
          <span role="alert" className="text-[12.5px] font-medium">{error}</span>
          <div className="flex gap-2">
            <Button type="button" variant="outline" size="sm" onClick={onClose} disabled={pending}>Cancel</Button>
            <Button type="submit" form="add-mcp-form" size="sm" disabled={pending}>{pending ? "Adding…" : "Add and check"}</Button>
          </div>
        </div>
      }
    >
      <form
        id="add-mcp-form"
        className="flex flex-col gap-4 px-6 pb-6"
        onSubmit={(e) => {
          e.preventDefault();
          const fd = new FormData(e.currentTarget);
          setError(null);
          start(async () => {
            const res = await createMcpServerAction(fd);
            if (!res.ok) return setError(res.error);
            onAdded(String(fd.get("name") ?? "").trim());
          });
        }}
      >
        <Field label="Name" name="name" required placeholder="github" hint="Models grant it by this name, and clients reach it at /mcp/<name>." />
        <Field label="Upstream URL" name="upstream_url" type="url" required placeholder="https://example.org/mcp" />
        <Field label="Authorization (optional)" name="auth_header" type="password" autoComplete="off" placeholder="Bearer …" hint="Sent to the server on every call. It's stored encrypted and never shown again." />
      </form>
    </Sheet>
  );
}

export function McpList({ servers, models, initialStats }: { servers: McpServer[]; models: ModelRoute[]; initialStats: DailyStatsView | null }) {
  const router = useRouter();
  const { probes, check, checkAll } = useProbes(servers);
  const stats = useQuery({ queryKey: ["daily-stats", "mcp"], queryFn: () => getJson<DailyStatsView>("/api/live/stats/daily?kind=mcp&days=7"), initialData: initialStats ?? undefined, refetchInterval: 60_000 });
  const [adding, setAdding] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const rows = useMemo(() => buildMcpRows(servers, models, probes, stats.data), [servers, models, probes, stats.data]);
  const down = rows.filter((r) => r.state === "not-answering");
  const tools = rows.reduce((n, r) => n + (r.tools ?? 0), 0);
  const calls = rows.reduce((n, r) => n + r.usage.toolCalls + r.usage.direct, 0);
  const errors = rows.reduce((n, r) => n + r.usage.errors, 0);
  const users = new Set(rows.flatMap((r) => r.usedBy));
  const openRow = rows.find((r) => r.server.id === open) ?? null;

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-[18px]">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          <h1 className="text-[26px] font-semibold tracking-tight">MCP servers</h1>
          <p className="text-[13px] text-secondary-foreground">{mcpLine(rows)}</p>
        </div>
        <Button type="button" size="sm" className="h-9" onClick={() => setAdding(true)}><Plus className="h-4 w-4" />Add server</Button>
      </div>
      <p className="max-w-[900px] text-[13px] leading-relaxed text-secondary-foreground">
        Tool servers your models can call. A model you grant a server gets its tools; obleth runs the calls in the gateway and hands the results back to the model. Clients with a key can also reach a server directly at <span className="font-mono text-[12px]">/mcp/&lt;name&gt;</span>.
      </p>
      {notice && <Notice onDismiss={() => setNotice(null)}>{notice}</Notice>}

      {servers.length === 0 ? (
        <section className="flex flex-col gap-2 rounded-xl border border-dashed border-border px-6 py-6">
          <p className="text-[15px] font-semibold">No tool servers yet</p>
          <p className="max-w-[760px] text-[13px] leading-relaxed text-secondary-foreground">Add the URL of an MCP server (streamable HTTP). obleth checks it, lists its tools, and you choose which models may use them. Models need Function calling on to use tools.</p>
          <div><Button type="button" variant="outline" size="sm" className="mt-1.5" onClick={() => setAdding(true)}><Plus className="h-4 w-4" />Add server</Button></div>
        </section>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
            <Tile label="Servers" value={servers.length} detail={`${servers.filter((s) => s.enabled).length} on · ${servers.filter((s) => !s.enabled).length} off`} />
            <Tile
              label="Not answering"
              value={down.length}
              emphasis={down.length > 0}
              detail={down.length ? down.map((r) => `${r.server.name}: ${r.probe?.message ?? "no answer"}${r.usedBy.length ? ` · ${r.usedBy.length} model${r.usedBy.length === 1 ? "" : "s"} lose its tools` : ""}`).join(" · ") : "all enabled servers answered"}
            />
            <Tile label="Tools" value={tools} detail={`from the ${rows.filter((r) => r.tools).length} that answered`} />
            <Tile label="Calls · 7 days" value={compact(calls)} detail={calls ? `${errors ? `${compact(errors)} failed · ` : ""}used by ${users.size} model${users.size === 1 ? "" : "s"}` : `granted to ${users.size} model${users.size === 1 ? "" : "s"}`} />
          </div>

          <section aria-label="Servers" className="overflow-hidden rounded-xl border border-border bg-card">
            <div className="overflow-x-auto">
              <div className="min-w-[1000px]">
                <div className={cn("grid items-center gap-3.5 px-[18px] py-2.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground", COLS)}>
                  <span>Server</span><span>Status</span><span className="text-right">Tools</span><span>Used by</span><span className="text-right">Calls · 7d</span><span>Upstream</span><span className="text-right">Answered in</span>
                </div>
                {rows.map((r) => {
                  const ms = probeLatency(r.probe);
                  const calls7 = r.usage.toolCalls + r.usage.direct;
                  return (
                    <button
                      key={r.server.id}
                      type="button"
                      aria-label={`Open ${r.server.name}`}
                      onClick={() => setOpen(r.server.id)}
                      className={cn("grid min-h-[56px] w-full items-center gap-3.5 border-t border-border px-[18px] py-2 text-left text-[13px] hover:bg-muted/30", COLS, !r.server.enabled && "text-muted-foreground")}
                    >
                      <span className="flex min-w-0 flex-col">
                        <span className="truncate font-mono text-[12.5px] font-medium text-foreground">{r.server.name}</span>
                        <span className="truncate text-[11.5px] text-muted-foreground">{[r.probe?.serverInfo ? `${r.probe.serverInfo.name} ${r.probe.serverInfo.version}` : null, r.server.auth_header_set ? "key set" : "no key"].filter(Boolean).join(" · ")}</span>
                      </span>
                      <span className="flex min-w-0 flex-col items-start gap-0.5">
                        <McpStateMark state={r.state} />
                        {r.state === "not-answering" && <span className="max-w-full truncate text-[11.5px] text-muted-foreground" title={r.probe?.message}>{r.probe?.message}</span>}
                      </span>
                      <span className="text-right font-mono text-[12px]">{r.tools ?? "—"}</span>
                      <span className="truncate text-[12.5px]" title={r.usedBy.join(", ")}>{r.usedBy.length ? r.usedBy.join(", ") : <span className="text-muted-foreground">no model yet</span>}</span>
                      <span className="text-right font-mono text-[12px]">{calls7 ? compact(calls7) : "—"}{r.usage.errors ? <span className="text-muted-foreground"> · {compact(r.usage.errors)} failed</span> : null}</span>
                      <span className="truncate font-mono text-[11.5px] text-muted-foreground" title={r.server.upstream_url}>{r.server.upstream_url}</span>
                      <span className="text-right font-mono text-[12px]">{ms != null ? `${ms} ms` : "—"}</span>
                    </button>
                  );
                })}
              </div>
            </div>
            <div className="flex flex-wrap justify-between gap-2 border-t border-border px-[18px] py-3 text-[12.5px] text-muted-foreground">
              <span>Checked when you opened this page · <button type="button" onClick={checkAll} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">Check again</button></span>
              <span>Filled dot answering · half dot old-style server · dashed off · white pill needs you</span>
            </div>
          </section>
        </>
      )}

      <AddSheet
        key={adding ? "open" : "closed"}
        open={adding}
        onClose={() => setAdding(false)}
        onAdded={(name) => { setAdding(false); setNotice(`Added ${name}. Grant it to models from its panel.`); router.refresh(); }}
      />
      {openRow && (
        <ServerSheet
          key={openRow.server.id}
          row={openRow}
          models={toolCapable(models)}
          onClose={() => setOpen(null)}
          onCheck={() => void check(openRow.server.name)}
          onChanged={(text) => { if (text) setNotice(text); router.refresh(); }}
        />
      )}
    </div>
  );
}
