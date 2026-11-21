"use client";

import { useMemo, useState } from "react";
import Link from "next/link";
import { Button } from "@/components/ui/button";
import { Pill } from "@/components/ui/pill";
import { Tile } from "@/components/ui/tile";
import { Cell, DataRow, DataTable, RowTitle } from "@/components/ui/data-table";
import { FilterBar } from "@/components/ui/filter-bar";
import { CodeBlock, CopyButton } from "@/components/portal/copy-button";
import type { PortalModelSummary } from "@/components/portal/portal-model-summary";
import { PortalCard, PortalHeader, requestSnippets } from "@/components/portal/ui";
import type { Tenant } from "@/lib/obleth";
import { MODEL_TYPE_NAMES } from "@/lib/models-model";
import { parseTagLevel, tagsInclude } from "@/lib/utils";
import { compact, formatNumber } from "@/lib/format";

export type { PortalModelSummary };

/** The models the tenant may call, by the names a request uses. */
export function PortalModels({
  models,
  tenant,
  gatewayBase,
}: {
  models: PortalModelSummary[];
  tenant: Tenant | null;
  gatewayBase: string;
}) {
  const [query, setQuery] = useState("");
  const snippets = requestSnippets(gatewayBase, models[0]?.model_name ?? "model-name");
  const chat = models.filter((m) => m.model_type === "chat").length;
  const tools = models.filter((m) => m.supports_function_calling && m.supports_tool_choice).length;
  const vision = models.filter((m) => m.supports_vision || tagsInclude(m.tags, "vision")).length;
  const restricted = (tenant?.allowed_models?.length ?? 0) > 0;
  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return models;
    return models.filter((m) => [m.model_name, m.description, ...(m.aliases ?? []), ...(m.tags ?? [])].some((v) => v?.toLowerCase().includes(q)));
  }, [models, query]);

  return (
    <div className="flex flex-col gap-5">
      <PortalHeader title="Models" action={<Button asChild variant="outline" size="sm" className="h-9"><Link href="/portal/keys">Your keys</Link></Button>}>
        The models your team can call. Put a model&apos;s name in a request&apos;s <span className="font-mono text-[12px]">model</span> field.
      </PortalHeader>

      <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
        <Tile label="Models" value={formatNumber(models.length)} detail={restricted ? "the ones your team is allowed" : "every model that is on"} />
        <Tile label="Chat" value={formatNumber(chat)} detail="chat completions" />
        <Tile label="Use tools" value={formatNumber(tools)} detail="function calls and tool choice" />
        <Tile label="See images" value={formatNumber(vision)} detail="read images in a prompt" />
      </div>

      <PortalCard title="Send a request" description="Put one of your keys in OBLETH_API_KEY, then use any model name below.">
        <div className="grid gap-3 border-t border-border px-[18px] py-4 lg:grid-cols-2">
          <CodeBlock label="curl" code={snippets.curl} />
          <CodeBlock label="wget" code={snippets.wget} />
        </div>
      </PortalCard>

      <FilterBar search={{ value: query, onChange: setQuery, label: "Search models", placeholder: "Name, alias or what it is good at" }} />

      <DataTable
        label="Models"
        minWidth="min-w-[760px]"
        columns={[{ label: "Model" }, { label: "Type" }, { label: "Context", align: "right" }, { label: "Can" }, { label: "Good at" }, { label: <span className="sr-only">Copy</span> }]}
        empty={models.length ? `No model matches “${query}”.` : "No models are on for your team right now."}
        footer={shown.length > 0 && <span>{shown.length === models.length ? `${formatNumber(models.length)} models` : `${formatNumber(shown.length)} of ${formatNumber(models.length)} models`}</span>}
      >
        {shown.map((m) => <ModelRow key={m.id} model={m} />)}
      </DataTable>
    </div>
  );
}

function ModelRow({ model: m }: { model: PortalModelSummary }) {
  const can = abilities(m);
  const tags = (m.tags ?? []).map((t) => parseTagLevel(t).base);
  const sub = [(m.aliases?.length ?? 0) > 0 ? `also ${m.aliases.join(", ")}` : null, m.description || null].filter(Boolean).join(" · ");
  return (
    <DataRow>
      <Cell className="max-w-[22rem]"><RowTitle sub={sub || undefined}>{m.model_name}</RowTitle></Cell>
      <Cell className="whitespace-nowrap text-secondary-foreground">
        {MODEL_TYPE_NAMES[m.model_type] ?? m.model_type}
        {m.quantization && m.quantization !== "unknown" && <span className="text-muted-foreground"> · {m.quantization.toUpperCase()}</span>}
      </Cell>
      <Cell align="right" mono>{m.context_window > 0 ? compact(m.context_window) : "—"}</Cell>
      <Cell><span className="flex flex-wrap gap-1">{can.length ? can.map((c) => <Pill key={c}>{c}</Pill>) : <span className="text-muted-foreground">—</span>}</span></Cell>
      <Cell className="max-w-[14rem] truncate text-[12.5px] text-secondary-foreground" title={tags.join(" · ")}>{tags.length ? tags.join(" · ") : <span className="text-muted-foreground">—</span>}</Cell>
      <Cell><span className="flex justify-end"><CopyButton value={m.model_name} label="Copy name" variant="ghost" /></span></Cell>
    </DataRow>
  );
}

function abilities(m: PortalModelSummary): string[] {
  return [
    m.supports_function_calling && "tools",
    m.supports_response_schema && "JSON schema",
    m.supports_vision && "images",
    (m.boons?.length ?? 0) > 0 && `+ ${m.boons.join(", ")}`,
  ].filter((v): v is string => Boolean(v));
}
