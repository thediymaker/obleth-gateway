"use client";

import { useState } from "react";
import { ChevronRight } from "lucide-react";
import { cn } from "@/lib/utils";
import type { McpServerTestRow, McpTestResult, McpTestStatus } from "@/lib/charo/mcp/types";
import { Rail, MicroLabel } from "@/components/charo/rail";
import { AttentionPill, StatusDot, StatusPill, type StatusGlyph } from "@/components/ui/status";

const WORD: Record<McpTestStatus, string> = { pass: "passed", warn: "warning", fail: "failed", skip: "skipped" };
const GLYPH: Record<Exclude<McpTestStatus, "fail">, StatusGlyph> = { pass: "on", warn: "half", skip: "off" };

/** A probe's result in the shared status marks: failed is the attention pill. */
export function McpStatusPill({ status }: { status: McpTestStatus }) {
  return <StatusPill glyph={status === "fail" ? "off" : GLYPH[status]} attention={status === "fail"} className="h-5 px-2 text-[11px]">{WORD[status]}</StatusPill>;
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

function RowMark({ status }: { status: McpTestStatus }) {
  if (status === "fail") return <AttentionPill icon={false} className="h-4 shrink-0 px-1.5 text-[10px]">failed</AttentionPill>;
  return <StatusDot glyph={GLYPH[status]} label={WORD[status]} className="h-[7px] w-[7px]" />;
}

function Row({ row }: { row: McpServerTestRow }) {
  const [open, setOpen] = useState(false);
  return (
    <div>
      <button type="button" onClick={() => setOpen((o) => !o)} className="flex w-full items-center gap-2 py-[5px] text-left">
        <RowMark status={row.status} />
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
