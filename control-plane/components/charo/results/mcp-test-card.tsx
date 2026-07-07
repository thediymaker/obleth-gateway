"use client";

import { useState } from "react";
import { ChevronRight } from "lucide-react";
import { cn } from "@/lib/utils";
import type { McpServerTestRow, McpTestResult, McpTestStatus } from "@/lib/charo/mcp/types";

const PILL: Record<McpTestStatus, string> = {
  pass: "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400",
  warn: "bg-amber-500/15 text-amber-600 dark:text-amber-400",
  fail: "bg-destructive/15 text-destructive",
  skip: "bg-muted text-muted-foreground",
};

export function McpStatusPill({ status }: { status: McpTestStatus }) {
  return (
    <span className={cn("rounded-full px-2 py-0.5 text-[10px] font-bold uppercase", PILL[status])}>
      {status}
    </span>
  );
}

/** Expanded details for one probed server; also used by the MCP dashboard tab. */
export function McpTestRowDetails({ row }: { row: McpServerTestRow }) {
  return (
    <div className="space-y-1 text-xs text-muted-foreground">
      {row.serverInfo && (
        <p>
          {row.serverInfo.name} v{row.serverInfo.version}
          {row.protocolVersion && <> · protocol {row.protocolVersion}</>}
        </p>
      )}
      {row.latencyMs && (
        <p>
          initialize {row.latencyMs.initialize}ms
          {row.latencyMs.toolsList !== null && <> · tools/list {row.latencyMs.toolsList}ms</>}
        </p>
      )}
      {row.tools && row.tools.length > 0 && (
        <ul className="list-inside list-disc">
          {row.tools.map((t) => (
            <li key={t.name}>
              <span className="font-mono text-foreground">{t.name}</span>
              {t.description && <> — {t.description}</>}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function Row({ row }: { row: McpServerTestRow }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="border-b border-border last:border-0">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className="flex w-full items-center gap-2 py-2 text-left"
      >
        <McpStatusPill status={row.status} />
        <span className="text-sm font-medium">{row.server}</span>
        <span className="ml-auto truncate text-xs text-muted-foreground">{row.message}</span>
        <ChevronRight
          className={cn("h-4 w-4 shrink-0 text-muted-foreground transition-transform", open && "rotate-90")}
        />
      </button>
      {open && (
        <div className="pb-3">
          <McpTestRowDetails row={row} />
        </div>
      )}
    </div>
  );
}

export function McpTestCard({ data }: { data: unknown }) {
  const r = (data ?? {}) as Partial<McpTestResult>;
  const servers = Array.isArray(r.servers) ? r.servers : [];
  return (
    <div className="w-full rounded-lg border border-border bg-card p-3">
      <div className="mb-1 flex items-center justify-between gap-2">
        <div className="truncate text-sm font-semibold">MCP servers</div>
        <span className="rounded-full border border-border px-2 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground">
          mcp test
        </span>
      </div>
      {servers.length === 0 ? (
        <p className="py-2 text-sm text-muted-foreground">No MCP servers registered.</p>
      ) : (
        <div>
          {servers.map((s) => (
            <Row key={s.server} row={s} />
          ))}
        </div>
      )}
    </div>
  );
}
