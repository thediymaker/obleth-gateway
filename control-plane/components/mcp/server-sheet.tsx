"use client";

import { useCallback, useState, useTransition } from "react";
import { AlertCircle } from "lucide-react";
import { deleteMcpServerAction, saveMcpServerAction, setMcpServerEnabledAction, setModelToolServerAction } from "@/app/actions";
import { SettingsForm, type SaveResult } from "@/components/access/settings-form";
import { Glyph } from "@/components/deployments/ui";
import { Setting, Switch, TextField } from "@/components/models/fields";
import { Notice, Sheet } from "@/components/models/ui";
import { Pill } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { MCP_STATE_LABEL, probeLatency, type McpRow, type McpState } from "@/lib/mcp-model";
import type { ModelRoute } from "@/lib/obleth";
import { compact } from "@/lib/overview-model";
import { cn } from "@/lib/utils";

/** Monochrome: filled dot answering, half dot old-style or checking, dashed off, white pill not answering. */
export function McpStateMark({ state }: { state: McpState }) {
  if (state === "not-answering") {
    return <span className="inline-flex h-[22px] items-center gap-1 whitespace-nowrap rounded-full bg-foreground px-2 text-[11.5px] font-semibold text-background"><AlertCircle className="h-3 w-3" aria-hidden />{MCP_STATE_LABEL[state]}</span>;
  }
  const glyph = state === "answering" ? "on" : state === "off" ? "off" : "half";
  return <Pill><Glyph glyph={glyph} className="h-[7px] w-[7px]" />{MCP_STATE_LABEL[state]}</Pill>;
}

const SHOWN = 6;

export function ServerSheet({ row, models, onClose, onCheck, onChanged }: { row: McpRow; models: ModelRoute[]; onClose: () => void; onCheck: () => void; onChanged: (text?: string) => void }) {
  const { confirm, confirmElement } = useConfirm();
  const [pending, start] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const [allModels, setAllModels] = useState(false);
  const [allTools, setAllTools] = useState(false);
  const [granted, setGranted] = useState<Set<string>>(new Set(row.usedBy));
  const s = row.server;
  const ms = probeLatency(row.probe);
  const ordered = [...models].sort((a, b) => Number(granted.has(b.model_name)) - Number(granted.has(a.model_name)) || a.model_name.localeCompare(b.model_name));
  const shownModels = allModels ? ordered : ordered.slice(0, Math.max(SHOWN, granted.size));
  const tools = row.probe?.tools ?? [];
  const u = row.usage;

  function grant(model: ModelRoute, on: boolean) {
    setGranted((g) => { const n = new Set(g); if (on) n.add(model.model_name); else n.delete(model.model_name); return n; });
    start(async () => {
      const res = await setModelToolServerAction(model.id, s.name, on);
      if (!res.ok) {
        setError(res.error);
        setGranted((g) => { const n = new Set(g); if (on) n.delete(model.model_name); else n.add(model.model_name); return n; });
      } else onChanged();
    });
  }

  function toggle() {
    start(async () => {
      if (s.enabled) {
        const ok = await confirm({ title: `Turn off ${s.name}?`, description: `Its tools disappear from ${row.usedBy.length || "every"} model${row.usedBy.length === 1 ? "" : "s"} at once, and /mcp/${s.name} answers 403. Its grants are kept.`, confirmLabel: "Turn off" });
        if (!ok) return;
      }
      const res = await setMcpServerEnabledAction(s.id, !s.enabled);
      if (!res.ok) setError(res.error);
      else onChanged(`${s.name} is ${s.enabled ? "off" : "on"}.`);
    });
  }

  function remove() {
    start(async () => {
      const ok = await confirm({ title: `Remove ${s.name}?`, description: `${row.usedBy.length ? `${row.usedBy.length} model${row.usedBy.length === 1 ? " loses" : "s lose"} its tools. ` : ""}Its registration and key are deleted. This cannot be undone.`, confirmLabel: "Remove" });
      if (!ok) return;
      const res = await deleteMcpServerAction(s.id);
      if (!res.ok) return setError(res.error);
      onClose();
      onChanged(`Removed ${s.name}.`);
    });
  }

  const save = useCallback(async (data: FormData): Promise<SaveResult> => {
    const res = await saveMcpServerAction({
      id: s.id,
      name: String(data.get("name") ?? ""),
      upstreamUrl: String(data.get("upstream_url") ?? ""),
      authHeader: String(data.get("auth_header") ?? ""),
      clearAuth: data.get("clear_auth") === "on",
    });
    if (!res.ok) return { ok: false, error: res.error, saved: [] };
    onChanged(res.name && res.name !== s.name ? `Renamed to ${res.name}; every model that had it still does.` : undefined);
    onCheck();
    return { ok: true };
  }, [s.id, s.name, onChanged, onCheck]);
  const sectionOf = useCallback((n: string) => (["name", "upstream_url", "auth_header", "clear_auth"].includes(n) ? "server" : null), []);
  const labelOf = useCallback((n: string) => ({ name: "Name", upstream_url: "Upstream URL", auth_header: "Authorization", clear_auth: "Remove the key" })[n] ?? n, []);

  return (
    <>
      {confirmElement}
      <Sheet
        open
        onClose={onClose}
        title={s.name}
        width="w-[min(640px,100vw)]"
        description={
          <span className="flex flex-wrap items-center gap-2">
            <McpStateMark state={row.state} />
            {ms != null && <Pill>{ms} ms</Pill>}
            {row.probe?.serverInfo && <Pill>{row.probe.serverInfo.name} {row.probe.serverInfo.version}</Pill>}
            {row.probe?.protocolVersion && <Pill>protocol {row.probe.protocolVersion}</Pill>}
            <span>clients: <span className="font-mono">/mcp/{s.name}</span></span>
          </span>
        }
      >
        <div className="flex flex-col gap-5 px-6 pb-4">
          <div className="flex flex-wrap gap-2">
            <Button type="button" variant="outline" size="sm" disabled={pending || !s.enabled} onClick={onCheck}>Check again</Button>
            <Button type="button" variant="outline" size="sm" disabled={pending} onClick={toggle}>{s.enabled ? "Turn off" : "Turn on"}</Button>
            <Button type="button" variant="outline" size="sm" disabled={pending} onClick={remove}>Remove…</Button>
          </div>
          {error && <Notice strong onDismiss={() => setError(null)}>{error}</Notice>}
          {row.state === "not-answering" && <Notice strong>{row.probe?.message ?? "It didn't answer."} Models granted it get no tools from it until it answers.</Notice>}
          {row.state === "old-sse" && <Notice>{row.probe?.message} obleth speaks streamable HTTP; an old HTTP+SSE server may need upgrading before its tools work.</Notice>}

          <section aria-label="Use" className="grid grid-cols-4 gap-2">
            {[
              ["Tool calls · 7d", u.toolCalls ? compact(u.toolCalls) : "0"],
              ["Direct · 7d", u.direct ? compact(u.direct) : "0"],
              ["Failed", u.errors ? compact(u.errors) : "0"],
              ["Average", u.avgMs != null ? `${u.avgMs} ms` : "—"],
            ].map(([label, value]) => (
              <div key={label} className="flex flex-col rounded-lg border border-border px-3 py-2">
                <span className="text-[11px] text-muted-foreground">{label}</span>
                <span className="text-lg font-semibold tabular-nums">{value}</span>
              </div>
            ))}
          </section>

          <section aria-label="Models that can use it">
            <div className="flex flex-wrap items-baseline justify-between gap-2 pb-1">
              <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Models that can use it · {granted.size}</span>
              <span className="text-xs text-muted-foreground">Only models with Function calling on are listed</span>
            </div>
            {models.length === 0 && <p className="py-2 text-[12.5px] text-muted-foreground">No chat model has Function calling on yet.</p>}
            {shownModels.map((m) => (
              <div key={m.id} className="flex items-center justify-between gap-3 border-t border-border py-2 text-[13px] first:border-t-0">
                <span className="truncate font-mono text-[12.5px]">{m.model_name}</span>
                <Switch label={`${m.model_name} can use ${s.name}`} checked={granted.has(m.model_name)} disabled={pending} onChange={(on) => grant(m, on)} />
              </div>
            ))}
            {ordered.length > shownModels.length && <button type="button" onClick={() => setAllModels(true)} className="pt-1.5 text-xs text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">{ordered.length - shownModels.length} more models could use it</button>}
          </section>

          <section aria-label="Its tools">
            <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Its tools · {row.tools ?? "unknown until it answers"}</span>
            {(allTools ? tools : tools.slice(0, SHOWN)).map((t) => (
              <div key={t.name} className="grid grid-cols-[200px_minmax(0,1fr)] gap-3 border-t border-border py-2 text-[12.5px] first:mt-1">
                <span className="truncate font-mono">{t.name}</span>
                <span className="text-muted-foreground">{t.description || "—"}</span>
              </div>
            ))}
            {tools.length > SHOWN && !allTools && <button type="button" onClick={() => setAllTools(true)} className="pt-1.5 text-xs text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">{tools.length - SHOWN} more</button>}
          </section>
        </div>

        <SettingsForm id={s.id} sectionOf={sectionOf} labelOf={labelOf} save={save} bar="panel" ariaLabel={`${s.name} connection`}>
          <p className="px-6 pb-1 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Connection</p>
          <Setting label="Name" hint="Renaming keeps every model's grant; clients must use the new path." fields={["name"]} was={{ field: "name" }} className="px-6">
            <TextField name="name" label="Name" required defaultValue={s.name} mono />
          </Setting>
          <Setting label="Upstream URL" hint="The server's streamable-HTTP endpoint." fields={["upstream_url"]} was={{ field: "upstream_url" }} className="px-6">
            <TextField name="upstream_url" label="Upstream URL" type="url" required defaultValue={s.upstream_url} mono />
          </Setting>
          <Setting label="Authorization" hint={s.auth_header_set ? "Set. Type a new one to replace it; it's never shown." : "None. Sent to the server on every call."} fields={["auth_header", "clear_auth"]} className="px-6">
            <TextField name="auth_header" label="Authorization" type="password" autoComplete="off" defaultValue="" placeholder={s.auth_header_set ? "•••••••• (set)" : "Bearer …"} />
            {s.auth_header_set && <Switch name="clear_auth" label="Remove the key" defaultChecked={false}>Remove the stored key</Switch>}
          </Setting>
        </SettingsForm>
      </Sheet>
    </>
  );
}
