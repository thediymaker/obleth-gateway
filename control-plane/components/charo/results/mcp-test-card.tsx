"use client";

import { useState } from "react";
import { ChevronRight } from "lucide-react";
import { cn } from "@/lib/utils";
import type { McpServerTestRow, McpTestResult, McpTestStatus } from "@/lib/charo/mcp/types";
import { Rail, MicroLabel } from "@/components/charo/rail";

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

const DOT: Record<McpTestStatus, string> = {
  pass: "bg-emerald-400",
  warn: "bg-amber-400",
  fail: "bg-destructive",
  skip: "bg-muted-foreground/40",
};

function Row({ row }: { row: McpServerTestRow }) {
  const [open, setOpen] = useState(false);
  return (
    <div>
      <button type="button" onClick={() => setOpen((o) => !o)} className="flex w-full items-center gap-2 py-[5px] text-left">
        <span className={cn("h-[7px] w-[7px] shrink-0 rounded-full", DOT[row.status])} aria-label={row.status} />
        <span className="text-[13px] font-medium text-foreground/90">{row.server}</span>
        <span className="ml-auto truncate text-[11.5px] text-muted-foreground">{row.message}</span>
        <ChevronRight className={cn("h-3.5 w-3.5 shrink-0 text-muted-foreground/70 transition-transform", open && "rotate-90")} />
      </button>
      {open && <div className="mb-1.5 ml-[15px]"><McpTestRowDetails row={row} /></div>}
    </div>
  );
}

export function McpTestCard({ data }: { data: unknown }) {
  const r = (data ?? {}) as Partial<McpTestResult>;
  const servers = Array.isArray(r.servers) ? r.servers : [];
  return (
    <Rail>
      <div className="mb-1 flex items-baseline gap-2">
        <span className="text-[13px] font-semibold text-foreground">MCP servers</span>
        <MicroLabel className="ml-auto shrink-0">MCP test</MicroLabel>
      </div>
      {servers.length === 0 ? (
        <p className="text-[12px] text-muted-foreground">No MCP servers registered.</p>
      ) : (
        <div>{servers.map((s) => <Row key={s.server} row={s} />)}</div>
      )}
    </Rail>
  );
}
