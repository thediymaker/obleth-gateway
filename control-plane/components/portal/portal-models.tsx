"use client";

import Link from "next/link";
import {
  Boxes,
  Braces,
  Cpu,
  KeyRound,
  MessageSquare,
  Route,
  Sparkles,
  Tag,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { CodeBlock, CopyButton } from "@/components/portal/copy-button";
import type { ModelRoute, Tenant } from "@/lib/obleth";
import { formatNumber } from "@/lib/utils";

export function PortalModels({
  models,
  tenant,
  gatewayBase,
}: {
  models: ModelRoute[];
  tenant: Tenant | null;
  gatewayBase: string;
}) {
  const defaultModel = models[0]?.model_name ?? "model-name";
  const snippets = buildSnippets(gatewayBase, defaultModel);
  const chatModels = models.filter((model) => model.model_type === "chat").length;
  const toolReady = models.filter(
    (model) => model.supports_function_calling && model.supports_tool_choice,
  ).length;
  const vision = models.filter((model) => model.supports_vision || model.tags?.includes("vision")).length;
  const restricted = (tenant?.allowed_models?.length ?? 0) > 0;

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-3 lg:flex-row lg:items-end lg:justify-between">
        <div>
          <h1 className="text-lg font-semibold tracking-tight">Models</h1>
          <p className="text-sm text-muted-foreground">
            Tenant-ready model names for OpenAI-compatible requests.
          </p>
        </div>
        <Button asChild size="sm" variant="secondary">
          <Link href="/portal/keys">
            <KeyRound className="h-3.5 w-3.5" aria-hidden />
            Manage keys
          </Link>
        </Button>
      </div>

      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <StatCard icon={Boxes} label="Available" value={formatNumber(models.length)} hint={restricted ? "tenant allowlist" : "enabled routes"} />
        <StatCard icon={MessageSquare} label="Chat routes" value={formatNumber(chatModels)} hint="chat completions" />
        <StatCard icon={Braces} label="Tool ready" value={formatNumber(toolReady)} hint="functions and tool choice" />
        <StatCard icon={Sparkles} label="Vision" value={formatNumber(vision)} hint="native or routed" />
      </div>

      <Card>
        <CardHeader>
          <div>
            <CardTitle>Start a request</CardTitle>
            <CardDescription>
              Set `OBLETH_API_KEY` to one of your tenant keys, then use any model name below.
            </CardDescription>
          </div>
        </CardHeader>
        <CardContent className="grid gap-3 lg:grid-cols-2">
          <CodeBlock label="curl" code={snippets.curl} />
          <CodeBlock label="wget" code={snippets.wget} />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Available models</CardTitle>
          <CardDescription>
            {formatNumber(models.length)} route{models.length === 1 ? "" : "s"} visible to your tenant.
          </CardDescription>
        </CardHeader>
        <CardContent className="p-0">
          {models.length === 0 ? (
            <div className="px-6 py-12 text-center text-sm text-muted-foreground">
              No models are currently available.
            </div>
          ) : (
            <div className="text-sm">
              <div className="grid border-b border-border text-left text-xs text-muted-foreground md:grid-cols-[minmax(0,1.25fr)_minmax(0,1fr)_minmax(0,1fr)_auto]">
                <div className="px-6 py-3 font-medium">Model</div>
                <div className="hidden px-3 py-3 font-medium md:block">Capabilities</div>
                <div className="hidden px-3 py-3 font-medium md:block">Route</div>
                <div className="hidden px-3 py-3 font-medium md:block" />
              </div>
              <div className="space-y-3 px-4 py-4">
                {models.map((model) => (
                  <ModelRow key={model.id} model={model} />
                ))}
              </div>
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

function ModelRow({ model }: { model: ModelRoute }) {
  const capabilities = capabilityList(model);
  return (
    <div className="group relative overflow-hidden rounded-lg border border-border/70 bg-card/35 shadow-sm transition-colors hover:border-border hover:bg-muted/15">
      <div className="grid min-w-0 md:grid-cols-[minmax(0,1.25fr)_minmax(0,1fr)_minmax(0,1fr)_auto] md:items-center">
        <div className="min-w-0 px-5 py-4 md:pr-5">
          <div className="flex min-w-0 items-start gap-3">
            <span className="mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-md border border-border/70 bg-background/40 text-muted-foreground">
              <Cpu className="h-4 w-4" aria-hidden />
            </span>
            <div className="min-w-0 flex-1">
              <div className="flex min-w-0 items-center gap-2">
                <p className="truncate font-medium" title={model.model_name}>
                  {model.model_name}
                </p>
                <CopyButton value={model.model_name} label="Copy model name" size="icon" variant="ghost" />
              </div>
              {model.description && (
                <p className="mt-0.5 line-clamp-2 text-xs leading-snug text-muted-foreground" title={model.description}>
                  {model.description}
                </p>
              )}
              <div className="mt-2 flex flex-wrap items-center gap-1.5 md:hidden">
                <ModelBadges model={model} capabilities={capabilities} />
              </div>
            </div>
          </div>
        </div>

        <div className="hidden min-w-0 px-3 py-4 md:block">
          <div className="flex flex-wrap items-center gap-1.5">
            <ModelBadges model={model} capabilities={capabilities} />
          </div>
        </div>

        <div className="min-w-0 px-5 pb-4 md:px-3 md:py-4">
          <p className="truncate font-mono text-xs text-muted-foreground" title={model.upstream_model}>
            {model.upstream_model}
          </p>
          <p className="mt-0.5 truncate font-mono text-[11px] text-muted-foreground/70" title={model.api_base}>
            {middleTruncate(model.api_base, 52)}
          </p>
        </div>

        <div className="flex items-center px-5 pb-4 md:justify-end md:px-4 md:py-4">
          <CopyButton value={model.model_name} label="Copy" variant="outline" />
        </div>
      </div>
    </div>
  );
}

function ModelBadges({
  model,
  capabilities,
}: {
  model: ModelRoute;
  capabilities: string[];
}) {
  return (
    <>
      {model.model_type && model.model_type !== "chat" && (
        <Badge className="border-primary/40 bg-primary/15 text-[10px] text-primary">
          {model.model_type}
        </Badge>
      )}
      <Badge className="border-border bg-background/70 text-[10px] text-muted-foreground">
        {formatContext(model.context_window)} ctx
      </Badge>
      {capabilities.map((capability) => (
        <Badge key={capability} className="border-sky-500/35 bg-sky-500/10 text-[10px] text-sky-400">
          {capability}
        </Badge>
      ))}
      {(model.tags?.length ?? 0) > 0 && (
        <span className="inline-flex min-w-0 items-center gap-1 text-[11px] text-muted-foreground">
          <Tag className="h-3 w-3 shrink-0" aria-hidden />
          <span className="truncate">{model.tags.join(" / ")}</span>
        </span>
      )}
      {(model.boons?.length ?? 0) > 0 && (
        <span className="inline-flex min-w-0 items-center gap-1 text-[11px] text-amber-500">
          <Route className="h-3 w-3 shrink-0" aria-hidden />
          <span className="truncate">{model.boons.join(" / ")}</span>
        </span>
      )}
    </>
  );
}

function StatCard({
  icon: Icon,
  label,
  value,
  hint,
}: {
  icon: typeof Boxes;
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

function capabilityList(model: ModelRoute): string[] {
  return [
    model.supports_function_calling && "functions",
    model.supports_system_messages && "system",
    model.supports_response_schema && "schema",
    model.supports_tool_choice && "tools",
    model.supports_vision && "vision",
  ].filter((value): value is string => Boolean(value));
}

function formatContext(value: number) {
  if (!Number.isFinite(value) || value <= 0) return "unknown";
  if (value >= 1_000_000) return `${Math.round(value / 1_000_000)}M`;
  if (value >= 1_000) return `${Math.round(value / 1_000)}K`;
  return String(value);
}

function middleTruncate(value: string, maxLength = 48): string {
  const trimmed = value.trim();
  if (trimmed.length <= maxLength) return trimmed;
  const head = Math.ceil((maxLength - 3) / 2);
  const tail = Math.floor((maxLength - 3) / 2);
  return `${trimmed.slice(0, head)}...${trimmed.slice(-tail)}`;
}
