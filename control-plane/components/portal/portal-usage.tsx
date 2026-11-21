"use client";

import { StatusText } from "@/components/ui/status";
import { Tile } from "@/components/ui/tile";
import { Cell, DataRow, DataTable, RowTitle } from "@/components/ui/data-table";
import { CodeBlock, CopyButton } from "@/components/portal/copy-button";
import { PortalCard, PortalHeader } from "@/components/portal/ui";
import type { KeyUsageSummary, UsageAgg, UsageLogEntry } from "@/lib/obleth";
import { ago, clockOrDate, compact, dateTime, formatMs, formatNumber, money } from "@/lib/format";

/** What the tenant's keys did: totals, each key's last day, and the newest requests. */
export function PortalUsage({
  usage,
  keyUsage,
  recent,
  gatewayBase,
}: {
  usage: UsageAgg[];
  keyUsage: KeyUsageSummary[];
  recent: UsageLogEntry[];
  gatewayBase: string;
}) {
  const total = usage.reduce(
    (acc, row) => ({ requests: acc.requests + row.requests, input: acc.input + row.input_tokens, output: acc.output + row.output_tokens, tokens: acc.tokens + row.total_tokens }),
    { requests: 0, input: 0, output: 0, tokens: 0 },
  );
  const failed = recent.filter((row) => row.status_code >= 400).length;
  const spend = keyUsage.reduce((sum, row) => sum + Number(row.cost_usd ?? 0), 0);
  const usedKeys = keyUsage.filter((row) => Number(row.requests ?? 0) > 0).length;
  const now = Date.now();
  const headerSnippet = [
    `curl -i ${gatewayBase}/v1/chat/completions`,
    `  -H "Authorization: Bearer $OBLETH_API_KEY"`,
    `  -H "Content-Type: application/json"`,
    `  -H "X-Session-ID: notebook-demo"`,
    `  -d '{"model":"model-name","messages":[{"role":"user","content":"ping"}]}'`,
  ].join(" \\\n");

  return (
    <div className="flex flex-col gap-5">
      <PortalHeader title="Usage">What your team&apos;s keys sent, and what the gateway answered.</PortalHeader>

      <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
        <Tile label="Requests" value={compact(total.requests)} detail={`from ${formatNumber(usedKeys)} key${usedKeys === 1 ? "" : "s"} in the last 24 h`} />
        <Tile label="Tokens" value={compact(total.tokens)} detail={`${compact(total.input)} in · ${compact(total.output)} out`} />
        <Tile label="Spend · 24h" value={money(spend)} detail="estimated" />
        <Tile label="Failed" value={formatNumber(failed)} emphasis={failed > 0} detail={`of the newest ${formatNumber(recent.length)} requests`} />
      </div>

      <div className="grid gap-4 xl:grid-cols-[minmax(0,0.9fr)_minmax(0,1.1fr)]">
        <PortalCard title="Find a request later" description="Send a session id, and keep the x-obleth-request-id the gateway sends back: an administrator can find the request by it.">
          <div className="border-t border-border px-[18px] py-4">
            <CodeBlock label="curl" code={headerSnippet} />
          </div>
        </PortalCard>

        <PortalCard title="Each key · 24h" label="Each key">
          <DataTable
            framed={false}
            label="Each key"
            columns={[{ label: "Key" }, { label: "Requests", align: "right" }, { label: "Tokens", align: "right" }, { label: "Spend", align: "right" }, { label: "Last answer" }]}
            empty="No key sent a request in the last 24 hours."
          >
            {keyUsage.slice(0, 8).map((row) => (
              <DataRow key={row.key_id}>
                <Cell className="max-w-[14rem]"><RowTitle sub={`${row.key_id.slice(0, 8)} · ${ago(row.last_used_ms, now)}`}>{row.last_model || "no recent model"}</RowTitle></Cell>
                <Cell align="right" mono>{formatNumber(row.requests)}</Cell>
                <Cell align="right" mono>{compact(row.total_tokens)}</Cell>
                <Cell align="right" mono>{money(row.cost_usd)}</Cell>
                <Cell>{row.last_status_code > 0 ? <Answer code={row.last_status_code} /> : <span className="text-muted-foreground">—</span>}</Cell>
              </DataRow>
            ))}
          </DataTable>
        </PortalCard>
      </div>

      <DataTable
        label="Newest requests"
        minWidth="min-w-[820px]"
        columns={[{ label: "When" }, { label: "Answer" }, { label: "Model" }, { label: "Key" }, { label: "Tokens", align: "right" }, { label: "Took", align: "right" }, { label: "Request id" }]}
        empty="No requests in the last 24 hours."
        footer={recent.length > 0 && <span>The newest {formatNumber(recent.length)} request{recent.length === 1 ? "" : "s"} from the last 24 hours</span>}
      >
        {recent.map((row) => (
          <DataRow key={`${row.request_id}-${row.ts_ms}`}>
            <Cell className="whitespace-nowrap text-muted-foreground" title={dateTime(row.ts_ms)}>{clockOrDate(row.ts_ms, now)}</Cell>
            <Cell><Answer code={row.status_code} /></Cell>
            <Cell className="max-w-[16rem] truncate font-mono text-[12px]" title={row.model}>{row.model}</Cell>
            <Cell className="max-w-[12rem] truncate">{row.key_name || row.key_prefix || "—"}</Cell>
            <Cell align="right" mono>{formatNumber(row.total_tokens)}</Cell>
            <Cell align="right" mono>{formatMs(row.total_ms)}</Cell>
            <Cell>
              <span className="flex items-center gap-1 font-mono text-[12px] text-muted-foreground">
                {row.request_id.slice(0, 8)}
                <CopyButton value={row.request_id} label="Copy request id" size="icon" variant="ghost" className="h-6 w-6" />
              </span>
            </Cell>
          </DataRow>
        ))}
      </DataTable>
    </div>
  );
}

/** "OK", or the failure's status code as something needing a look. */
function Answer({ code }: { code: number }) {
  return code < 400 ? <StatusText glyph="on">OK · {code}</StatusText> : <StatusText glyph="on" attention>Failed · {code}</StatusText>;
}
