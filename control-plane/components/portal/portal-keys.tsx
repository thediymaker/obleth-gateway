"use client";

import { useMemo, useState, useTransition } from "react";
import { Plus } from "lucide-react";
import { createPortalKey, deletePortalKey, disablePortalKey } from "@/app/(portal)/portal-actions";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Notice } from "@/components/ui/notice";
import { Sheet } from "@/components/ui/sheet";
import { SecretBanner } from "@/components/ui/secret-banner";
import { StatusText } from "@/components/ui/status";
import { Tile } from "@/components/ui/tile";
import { Cell, DataRow, DataTable, RowTitle } from "@/components/ui/data-table";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { CodeBlock, CopyButton } from "@/components/portal/copy-button";
import { PortalCard, PortalHeader, requestSnippets } from "@/components/portal/ui";
import type { ApiKey, KeyUsageSummary } from "@/lib/obleth";
import type { ActionResult } from "@/lib/action-result";
import { describeIdentityKey } from "@/lib/key-kind";
import { ago, compact, formatNumber, money } from "@/lib/format";

/**
 * The tenant's own keys. Every action names a key by id only: the server
 * action takes the tenant from the session and checks the key is its own.
 */
export function PortalKeys({
  keys,
  keyUsage,
  gatewayBase,
  defaultModel,
}: {
  keys: ApiKey[];
  keyUsage: KeyUsageSummary[];
  gatewayBase: string;
  defaultModel: string;
}) {
  const [secret, setSecret] = useState<{ secret: string; name: string } | null>(null);
  const [making, setMaking] = useState(false);
  const [makeError, setMakeError] = useState<string | null>(null);
  const [rowErrors, setRowErrors] = useState<Record<string, string>>({});
  const [pending, start] = useTransition();
  const { confirm, confirmElement } = useConfirm();

  const usageByKey = useMemo(() => new Map(keyUsage.map((u) => [u.key_id, u])), [keyUsage]);
  const on = keys.filter((k) => !k.disabled).length;
  const sum = (pick: (u: KeyUsageSummary) => number) => keyUsage.reduce((n, u) => n + Number(pick(u) ?? 0), 0);
  const snippets = requestSnippets(gatewayBase, defaultModel);
  const now = Date.now();

  function make(formData: FormData) {
    const name = String(formData.get("name") ?? "");
    setMakeError(null);
    start(async () => {
      const result = await createPortalKey(name);
      if (result.ok) {
        setSecret({ secret: result.secret, name: name.trim() });
        setMaking(false);
      } else {
        setMakeError(result.error);
      }
    });
  }

  function run(key: ApiKey, action: () => Promise<ActionResult>) {
    setRowErrors((prev) => ({ ...prev, [key.id]: "" }));
    start(async () => {
      const result = await action();
      if (!result.ok) setRowErrors((prev) => ({ ...prev, [key.id]: result.error }));
    });
  }

  async function turnOff(key: ApiKey) {
    const ok = await confirm({
      title: `Turn off ${key.name}?`,
      description: "Requests with it are refused straight away. Only an administrator can turn it back on.",
      confirmLabel: "Turn off",
    });
    if (ok) run(key, () => disablePortalKey(key.id));
  }

  async function remove(key: ApiKey) {
    const ok = await confirm({
      tone: "destructive",
      title: `Delete ${key.name}?`,
      description: "Requests with it fail at once. Its past requests stay in your usage. This cannot be undone.",
      confirmLabel: "Delete",
    });
    if (ok) run(key, () => deletePortalKey(key.id));
  }

  return (
    <div className="flex flex-col gap-5">
      {confirmElement}
      <PortalHeader title="Keys" action={<Button type="button" size="sm" className="h-9" onClick={() => { setMakeError(null); setMaking(true); }}><Plus className="h-4 w-4" />Make key</Button>}>
        Your team&apos;s keys for calling the gateway. A key works with any OpenAI-compatible client.
      </PortalHeader>

      {secret && <SecretBanner title={`${secret.name || "Your new key"}: copy it now, it is shown only once.`} secret={secret.secret} env="OBLETH_API_KEY" onDone={() => setSecret(null)} />}

      <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
        <Tile label="Keys" value={formatNumber(keys.length)} detail={`${formatNumber(on)} on · ${formatNumber(keys.length - on)} off`} />
        <Tile label="Requests · 24h" value={compact(sum((u) => u.requests))} />
        <Tile label="Tokens · 24h" value={compact(sum((u) => u.total_tokens))} />
        <Tile label="Spend · 24h" value={money(sum((u) => u.cost_usd))} detail="estimated" />
      </div>

      <PortalCard title="Use a key" description="Put the key in OBLETH_API_KEY, then call the gateway.">
        <div className="grid gap-3 border-t border-border px-[18px] py-4 lg:grid-cols-2">
          <CodeBlock label="curl" code={snippets.curl} />
          <CodeBlock label="wget" code={snippets.wget} />
        </div>
      </PortalCard>

      <DataTable
        label="Keys"
        minWidth="min-w-[760px]"
        columns={[{ label: "Key" }, { label: "State" }, { label: "Requests · 24h", align: "right" }, { label: "Tokens · 24h", align: "right" }, { label: "Spend · 24h", align: "right" }, { label: "Last used" }, { label: <span className="sr-only">Actions</span> }]}
        empty="No keys yet. Make one to send your first request."
        footer={keys.length > 0 && <span>{formatNumber(keys.length)} key{keys.length === 1 ? "" : "s"} in your team</span>}
      >
        {keys.map((k) => {
          const u = usageByKey.get(k.id);
          const identity = describeIdentityKey(k);
          return (
            <DataRow key={k.id} muted={k.disabled}>
              <Cell className="max-w-[18rem]">
                <RowTitle
                  muted={k.disabled}
                  sub={identity.isIdentity ? `identity · ${identity.subject} · ${identity.issuerHost}` : <span className="inline-flex items-center gap-1">{k.key_prefix}…<CopyButton value={k.key_prefix} label="Copy prefix" size="icon" variant="ghost" className="h-6 w-6" /></span>}
                >
                  {k.name}
                </RowTitle>
                {rowErrors[k.id] && <p role="alert" className="mt-1 text-xs font-medium text-foreground">{rowErrors[k.id]}</p>}
              </Cell>
              <Cell><StatusText glyph={k.disabled ? "off" : "on"}>{k.disabled ? "Off" : "On"}</StatusText></Cell>
              <Cell align="right" mono>{formatNumber(Number(u?.requests ?? 0))}</Cell>
              <Cell align="right" mono>{compact(Number(u?.total_tokens ?? 0))}</Cell>
              <Cell align="right" mono>{money(Number(u?.cost_usd ?? 0))}</Cell>
              <Cell className="whitespace-nowrap text-muted-foreground">{ago(u?.last_used_ms, now)}</Cell>
              <Cell>
                <span className="flex justify-end gap-1">
                  {/* Turning a key back on is an administrator's call. */}
                  {!k.disabled && <Button type="button" size="sm" variant="ghost" disabled={pending} onClick={() => void turnOff(k)}>Turn off…</Button>}
                  <Button type="button" size="sm" variant="ghost" disabled={pending} onClick={() => void remove(k)}>Delete…</Button>
                </span>
              </Cell>
            </DataRow>
          );
        })}
      </DataTable>

      <Sheet
        open={making}
        onClose={() => { if (!pending) setMaking(false); }}
        title="Make a key"
        description="Name it after what will use it, so you can tell your keys apart."
        width="w-[min(480px,100vw)]"
      >
        <form action={make} className="flex flex-col gap-4 px-6 pb-6">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="new-key-name">Name</Label>
            <Input id="new-key-name" name="name" placeholder="research-notebook" required disabled={pending} />
          </div>
          {makeError && <Notice strong>{makeError}</Notice>}
          <div className="flex justify-end gap-2">
            <Button type="button" variant="ghost" onClick={() => setMaking(false)} disabled={pending}>Cancel</Button>
            <Button type="submit" disabled={pending}>{pending ? "Making…" : "Make key"}</Button>
          </div>
        </form>
      </Sheet>
    </div>
  );
}
