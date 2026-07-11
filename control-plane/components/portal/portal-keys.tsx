"use client";

import { useMemo, useState, useTransition } from "react";
import {
  Activity,
  CheckCircle2,
  KeyRound,
  Plus,
  Power,
  ShieldCheck,
  Trash2,
} from "lucide-react";
import {
  createPortalKey,
  deletePortalKey,
  disablePortalKey,
} from "@/app/(portal)/portal-actions";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { CodeBlock, CopyButton } from "@/components/portal/copy-button";
import type { ApiKey, KeyUsageSummary } from "@/lib/obleth";
import { cn, formatCurrency, formatNumber } from "@/lib/utils";

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
  const [secret, setSecret] = useState<string | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const [rowErrors, setRowErrors] = useState<Record<string, string>>({});
  const [pending, start] = useTransition();
  const { confirm, confirmElement } = useConfirm();

  const usageByKey = useMemo(
    () => new Map(keyUsage.map((usage) => [usage.key_id, usage])),
    [keyUsage],
  );
  const activeCount = keys.filter((key) => !key.disabled).length;
  const totalRequests = keyUsage.reduce((sum, usage) => sum + Number(usage.requests ?? 0), 0);
  const totalTokens = keyUsage.reduce((sum, usage) => sum + Number(usage.total_tokens ?? 0), 0);
  const totalCost = keyUsage.reduce((sum, usage) => sum + Number(usage.cost_usd ?? 0), 0);
  const snippets = buildSnippets(gatewayBase, defaultModel);

  function handleCreate(formData: FormData) {
    setCreateError(null);
    start(async () => {
      const result = await createPortalKey(formData);
      if (result.ok) {
        setSecret(result.secret);
        setCreateOpen(false);
      } else {
        setCreateError(result.error);
      }
    });
  }

  function handleToggle(key: ApiKey) {
    setRowErrors((prev) => ({ ...prev, [key.id]: "" }));
    start(async () => {
      const fd = new FormData();
      fd.set("id", key.id);
      fd.set("disabled", String(!key.disabled));
      const result = await disablePortalKey(fd);
      if (!result.ok) {
        setRowErrors((prev) => ({ ...prev, [key.id]: result.error }));
      }
    });
  }

  async function handleDelete(key: ApiKey) {
    const ok = await confirm({
      title: "Delete API key",
      description: `Delete API key "${key.name}"? Clients using it stop authenticating immediately. This cannot be undone.`,
    });
    if (!ok) return;
    setRowErrors((prev) => ({ ...prev, [key.id]: "" }));
    start(async () => {
      const fd = new FormData();
      fd.set("id", key.id);
      const result = await deletePortalKey(fd);
      if (!result.ok) {
        setRowErrors((prev) => ({ ...prev, [key.id]: result.error }));
      }
    });
  }

  return (
    <div className="space-y-6">
      {confirmElement}
      <div className="flex flex-col gap-3 lg:flex-row lg:items-end lg:justify-between">
        <div>
          <h1 className="text-lg font-semibold tracking-tight">Keys</h1>
          <p className="text-sm text-muted-foreground">
            Tenant API keys for OpenAI-compatible gateway requests.
          </p>
        </div>

        <Dialog open={createOpen} onOpenChange={setCreateOpen}>
          <DialogTrigger asChild>
            <Button size="sm">
              <Plus className="h-3.5 w-3.5" aria-hidden />
              New key
            </Button>
          </DialogTrigger>
          <DialogContent className="max-w-md">
            <DialogHeader>
              <DialogTitle>Create API key</DialogTitle>
              <DialogDescription>
                Give the key a memorable name for the workload that will use it.
              </DialogDescription>
            </DialogHeader>
            <form action={handleCreate} className="space-y-4">
              <div className="space-y-1.5">
                <Label htmlFor="new-key-name">Key name</Label>
                <Input id="new-key-name" name="name" placeholder="research-notebook" required />
              </div>
              {createError && (
                <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
                  {createError}
                </p>
              )}
              <DialogFooter>
                <Button type="button" variant="ghost" onClick={() => setCreateOpen(false)} disabled={pending}>
                  Cancel
                </Button>
                <Button type="submit" disabled={pending}>
                  <KeyRound className="h-4 w-4" aria-hidden />
                  {pending ? "Creating..." : "Create key"}
                </Button>
              </DialogFooter>
            </form>
          </DialogContent>
        </Dialog>
      </div>

      {secret && (
        <Card>
          <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
            <div className="min-w-0">
              <CardTitle>Your new API key</CardTitle>
              <CardDescription>Copy this secret now. It will not be shown again.</CardDescription>
            </div>
            <div className="flex shrink-0 items-center gap-2">
              <CopyButton value={secret} label="Copy key" variant="secondary" />
              <Button variant="ghost" size="sm" onClick={() => setSecret(null)}>
                Dismiss
              </Button>
            </div>
          </CardHeader>
          <CardContent>
            <div className="flex min-w-0 items-center gap-2 rounded-md border border-border bg-background px-3 py-2">
              <code className="min-w-0 flex-1 break-all font-mono text-xs">{secret}</code>
            </div>
          </CardContent>
        </Card>
      )}

      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <StatCard icon={KeyRound} label="Keys" value={formatNumber(keys.length)} hint={`${formatNumber(activeCount)} active`} />
        <StatCard icon={Activity} label="Requests" value={formatNumber(totalRequests)} hint="recent window" />
        <StatCard icon={CheckCircle2} label="Tokens" value={formatNumber(totalTokens)} hint="input and output" />
        <StatCard icon={ShieldCheck} label="Cost" value={formatCurrency(totalCost)} hint="estimated" />
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Use a key</CardTitle>
          <CardDescription>
            Create a key, export it as `OBLETH_API_KEY`, then call the gateway.
          </CardDescription>
        </CardHeader>
        <CardContent className="grid gap-3 lg:grid-cols-2">
          <CodeBlock label="curl" code={snippets.curl} />
          <CodeBlock label="wget" code={snippets.wget} />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>API keys</CardTitle>
          <CardDescription>
            {formatNumber(keys.length)} key{keys.length === 1 ? "" : "s"} scoped to your tenant.
          </CardDescription>
        </CardHeader>

        <CardContent className="p-0">
          {keys.length === 0 ? (
            <div className="px-6 py-12 text-center text-sm text-muted-foreground">
              No keys yet. Create one to make your first request.
            </div>
          ) : (
            <div className="space-y-3 px-4 py-4">
              {keys.map((key) => (
                <KeyRow
                  key={key.id}
                  apiKey={key}
                  usage={usageByKey.get(key.id)}
                  error={rowErrors[key.id]}
                  pending={pending}
                  onToggle={() => handleToggle(key)}
                  onDelete={() => handleDelete(key)}
                />
              ))}
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

function KeyRow({
  apiKey,
  usage,
  error,
  pending,
  onToggle,
  onDelete,
}: {
  apiKey: ApiKey;
  usage?: KeyUsageSummary;
  error?: string;
  pending: boolean;
  onToggle: () => void;
  onDelete: () => void;
}) {
  const active = !apiKey.disabled;

  return (
    <div
      className={cn(
        "rounded-lg border shadow-sm transition-colors",
        active
          ? "border-border/70 bg-card/35 hover:border-border hover:bg-muted/15"
          : "border-border/60 bg-muted/10 opacity-85",
      )}
    >
      <div className="grid gap-3 px-5 py-4 lg:grid-cols-[minmax(0,1.35fr)_minmax(18rem,1fr)_auto] lg:items-center">
        <div className="min-w-0">
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            <p className="truncate font-medium" title={apiKey.name}>
              {apiKey.name}
            </p>
            <Badge className={active ? "border-emerald-500/40 bg-emerald-500/10 text-emerald-500" : "opacity-60"}>
              {active ? "active" : "disabled"}
            </Badge>
          </div>
          <div className="mt-1 flex min-w-0 flex-wrap items-center gap-2">
            <span className="font-mono text-xs text-muted-foreground">{apiKey.key_prefix}...</span>
            <CopyButton value={apiKey.key_prefix} label="Copy prefix" size="icon" variant="ghost" />
          </div>
          {error && <p className="mt-1 text-xs text-destructive">{error}</p>}
        </div>

        <div className="grid grid-cols-2 gap-2 text-xs sm:grid-cols-4 lg:grid-cols-2 xl:grid-cols-4">
          <MiniMetric label="Requests" value={formatNumber(Number(usage?.requests ?? 0))} />
          <MiniMetric label="Tokens" value={formatNumber(Number(usage?.total_tokens ?? 0))} />
          <MiniMetric label="Cost" value={formatCurrency(Number(usage?.cost_usd ?? 0))} />
          <MiniMetric label="Last used" value={formatLastUsed(usage?.last_used_ms)} />
        </div>

        <div className="flex flex-wrap items-center gap-2 lg:justify-end">
          <Button variant="outline" size="sm" disabled={pending} onClick={onToggle}>
            <Power className="h-3.5 w-3.5" aria-hidden />
            {active ? "Disable" : "Enable"}
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={pending}
            onClick={onDelete}
            className="border-destructive/40 text-destructive hover:bg-destructive/10 hover:text-destructive"
          >
            <Trash2 className="h-3.5 w-3.5" aria-hidden />
            Delete
          </Button>
        </div>
      </div>
    </div>
  );
}

function StatCard({
  icon: Icon,
  label,
  value,
  hint,
}: {
  icon: typeof KeyRound;
  label: string;
  value: string;
  hint: string;
}) {
  return (
    <div className="rounded-lg border border-border bg-card p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="text-xs text-muted-foreground">{label}</p>
          <p className="mt-1 text-2xl font-semibold tabular-nums">{value}</p>
          <p className="mt-0.5 text-[11px] text-muted-foreground">{hint}</p>
        </div>
        <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-border/70 bg-background/40 text-muted-foreground">
          <Icon className="h-4 w-4" aria-hidden />
        </span>
      </div>
    </div>
  );
}

function MiniMetric({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 rounded-md border border-border/70 bg-background/35 px-2.5 py-2">
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      <p className="mt-0.5 truncate text-xs font-medium tabular-nums">{value}</p>
    </div>
  );
}

function buildSnippets(gatewayBase: string, modelName: string) {
  const payload = JSON.stringify({
    model: modelName,
    messages: [{ role: "user", content: "Say hello from obleth." }],
  });

  return {
    curl: [
      `curl ${gatewayBase}/v1/chat/completions`,
      `  -H "Authorization: Bearer $OBLETH_API_KEY"`,
      `  -H "Content-Type: application/json"`,
      `  -d '${payload}'`,
    ].join(" \\\n"),
    wget: [
      `wget -qO- ${gatewayBase}/v1/chat/completions`,
      `  --header="Authorization: Bearer $OBLETH_API_KEY"`,
      `  --header="Content-Type: application/json"`,
      `  --post-data='${payload}'`,
    ].join(" \\\n"),
  };
}

function formatLastUsed(ms?: number): string {
  if (!ms || ms <= 0) return "Never";
  const diff = Date.now() - ms;
  if (diff < 0) return "Just now";
  const min = Math.floor(diff / 60_000);
  if (min < 1) return "Just now";
  if (min < 60) return `${min}m ago`;
  const hours = Math.floor(min / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}d ago`;
  return new Date(ms).toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" });
}
