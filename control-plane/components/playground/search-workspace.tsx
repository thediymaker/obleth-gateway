"use client";

import { useState } from "react";
import { Copy, ExternalLink, Loader2 } from "lucide-react";
import Link from "next/link";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";
import type { ModelRoute } from "@/lib/obleth";
import { cn } from "@/lib/utils";
import type { PlaygroundSession } from "./playground";
import { Segmented } from "./ui";

export interface SearchResult {
  title: string;
  url: string;
  snippet: string;
  date: string | null;
}

interface RunResult {
  tool: string;
  results: SearchResult[];
  latencyMs: number;
  requestId: string | null;
}

const TIME_RANGES = [
  { value: "", label: "Any time" },
  { value: "day", label: "Day" },
  { value: "week", label: "Week" },
  { value: "month", label: "Month" },
  { value: "year", label: "Year" },
] as const;

export const DEFAULT_MAX_RESULTS = 5;

const fieldCls = "w-full rounded-lg border border-border bg-background px-2.5 py-2 text-[13px] outline-none placeholder:text-muted-foreground focus:ring-1 focus:ring-ring";

/** The domain filter as typed (one per line, or comma separated) to the list the gateway takes. */
export function domainList(text: string): string[] {
  const out: string[] = [];
  for (const raw of text.split(/[\n,]/)) {
    const d = raw.trim();
    if (d && !out.includes(d)) out.push(d);
  }
  return out.slice(0, 20);
}

/** The host a result links to, for the line under its title. */
function hostOf(url: string): string {
  try {
    return new URL(url).host.replace(/^www\./, "");
  } catch {
    return url;
  }
}

function PaneHeader({ step, title, children }: { step: number; title: React.ReactNode; children?: React.ReactNode }) {
  return (
    <div className="flex h-12 shrink-0 items-center justify-between gap-2 border-b border-border pl-4 pr-3">
      <span className="flex items-center gap-2 text-[13px] font-semibold"><span className="text-[11px] font-medium text-muted-foreground">{step}</span>{title}</span>
      {children}
    </div>
  );
}

/**
 * The Search mode of the Playground: run a web search through one of the
 * gateway's search tools (registry routes of type `search`) and read the
 * results as a client would get them. Calls the gateway's /v1/search as the
 * reserved internal tenant via /api/live/playground/search.
 */
export function SearchWorkspace({ session, update, models, loading }: {
  session: PlaygroundSession;
  update: (patch: Partial<PlaygroundSession>) => void;
  models: ModelRoute[];
  loading: boolean;
}) {
  const tools = models.filter((m) => m.model_type === "search" && m.enabled);
  const tool = session.searchTool && tools.some((t) => t.model_name === session.searchTool) ? session.searchTool : tools[0]?.model_name ?? "";
  const query = session.searchQuery ?? "";
  const maxResults = session.searchMaxResults ?? DEFAULT_MAX_RESULTS;
  const domainsText = session.searchDomains ?? "";
  const timeRange = session.searchTimeRange ?? "";

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<RunResult | null>(null);

  const canRun = !busy && tool !== "" && query.trim().length > 0;

  const run = async () => {
    if (!canRun) return;
    setBusy(true);
    setError(null);
    try {
      const domains = domainList(domainsText);
      const res = await fetch("/api/live/playground/search", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          tool,
          query: query.trim(),
          maxResults,
          ...(domains.length ? { domains } : {}),
          ...(timeRange ? { timeRange } : {}),
        }),
      });
      const json = await res.json().catch(() => null);
      if (!res.ok) {
        throw new Error(json?.error || `Search failed (HTTP ${res.status}).`);
      }
      setResult({ tool, results: json?.results ?? [], latencyMs: json?.latencyMs ?? 0, requestId: json?.requestId ?? null });
    } catch (e) {
      setResult(null);
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className="flex h-full min-h-0 flex-col overflow-y-auto lg:flex-row lg:overflow-hidden"
      onKeyDown={(e) => { if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); void run(); } }}
    >
      <section aria-label="Search" className="flex flex-col border-b border-border lg:w-[380px] lg:shrink-0 lg:border-b-0 lg:border-r">
        <PaneHeader step={1} title="Search" />
        <div className="min-h-0 flex-1 space-y-4 overflow-y-auto p-4">
          {!loading && tools.length === 0 ? (
            <p className="rounded-lg border border-dashed border-border p-4 text-[13px] leading-relaxed text-muted-foreground">
              No search tool is registered yet. Add one on the <Link href="/models" className="underline underline-offset-2 hover:text-foreground">Models</Link> page with the type &ldquo;Web search (SearXNG)&rdquo; and the address of a SearXNG instance, then come back here to try it.
            </p>
          ) : (
            <>
              <label className="block space-y-1.5">
                <span className="text-[12.5px] font-medium text-secondary-foreground">Search tool</span>
                <Select aria-label="Search tool" value={tool} onValueChange={(value) => update({ searchTool: value })}
                  options={tools.map((t) => ({ value: t.model_name, label: t.model_name }))} />
              </label>
              <label className="block space-y-1.5">
                <span className="text-[12.5px] font-medium text-secondary-foreground">Query</span>
                <textarea aria-label="Query" value={query} maxLength={2000} rows={3}
                  placeholder="attention is all you need"
                  className={cn(fieldCls, "resize-y")}
                  onChange={(e) => update({ searchQuery: e.target.value })} />
              </label>
              <div className="space-y-1.5">
                <span className="text-[12.5px] font-medium text-secondary-foreground">Results to return</span>
                <Select aria-label="Results to return" value={String(maxResults)} onValueChange={(value) => update({ searchMaxResults: Number(value) })}
                  options={[1, 3, 5, 10, 20].map((n) => ({ value: String(n), label: String(n) }))} />
              </div>
              <div className="space-y-1.5">
                <span className="text-[12.5px] font-medium text-secondary-foreground">Published within</span>
                <Segmented label="Published within" value={timeRange} options={TIME_RANGES.map((t) => ({ value: t.value, label: t.label }))}
                  onChange={(value) => update({ searchTimeRange: value === "" ? undefined : (value as PlaygroundSession["searchTimeRange"]) })} />
              </div>
              <label className="block space-y-1.5">
                <span className="text-[12.5px] font-medium text-secondary-foreground">Only these sites (optional)</span>
                <textarea aria-label="Domain filter" value={domainsText} rows={3}
                  placeholder={"arxiv.org\nnature.com\n-pinterest.com"}
                  className={cn(fieldCls, "resize-y font-mono text-xs")}
                  onChange={(e) => update({ searchDomains: e.target.value })} />
                <span className="block text-[11.5px] leading-snug text-muted-foreground">One site per line. Results are kept to these sites and their subdomains. Put a minus in front of a site to leave it out instead.</span>
              </label>
            </>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2 border-t border-border px-4 py-3">
          {loading && <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" aria-label="Loading models" />}
          <span className="ml-auto hidden text-xs text-muted-foreground sm:inline">Ctrl + Enter</span>
          <Button disabled={!canRun} onClick={() => void run()} aria-label="Run search">
            {busy && <Loader2 className="h-4 w-4 animate-spin" />}
            Search
          </Button>
        </div>
      </section>

      <section aria-label="Results" className="flex min-h-[16rem] min-w-0 flex-1 flex-col bg-card">
        <PaneHeader step={2} title="Results">
          {result && (
            <Button variant="ghost" size="sm" className="h-7 text-xs text-muted-foreground" onClick={() => void navigator.clipboard.writeText(JSON.stringify({ object: "search", results: result.results }, null, 2)).catch(() => {})}>
              <Copy className="h-3.5 w-3.5" />Copy JSON
            </Button>
          )}
        </PaneHeader>
        <div className="min-h-0 flex-1 space-y-4 overflow-y-auto p-4">
          {error && <p role="alert" className="rounded-lg border border-border bg-secondary/50 p-3 text-sm">{error}</p>}
          {!result && !error && (
            <p className="rounded-xl border border-dashed border-border p-5 text-center text-[13px] leading-relaxed text-muted-foreground">
              {busy ? "Searching…" : <>Results appear here exactly as a client of <code className="font-mono">/v1/search</code> receives them: a title, the link, a short snippet and, when the source gives one, a date.</>}
            </p>
          )}
          {result && (
            <>
              <div className="flex flex-wrap gap-x-3 gap-y-1 font-mono text-[11.5px] text-muted-foreground">
                <span className="text-foreground">{result.tool}</span>
                <span>{result.results.length} result{result.results.length === 1 ? "" : "s"}</span>
                <span>{result.latencyMs} ms</span>
                {result.requestId && <span title="Request id">{result.requestId}</span>}
              </div>
              {result.results.length === 0 ? (
                <p className="rounded-lg border border-border p-4 text-[13px] leading-relaxed text-muted-foreground">
                  The search ran but found nothing. Try a broader query or fewer site filters. If every query comes back empty, the search engines behind the tool may be refusing its requests; its health check on the Models page will say whether the tool itself answers.
                </p>
              ) : (
                <ol className="space-y-3">
                  {result.results.map((r, i) => (
                    <li key={`${r.url}-${i}`} className="rounded-xl border border-border bg-background p-3.5">
                      <a href={r.url} target="_blank" rel="noopener noreferrer" className="group inline-flex items-start gap-1.5 text-[13.5px] font-semibold leading-snug hover:underline">
                        {r.title || r.url}
                        <ExternalLink className="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground opacity-0 group-hover:opacity-100" aria-hidden="true" />
                      </a>
                      <p className="mt-0.5 flex flex-wrap gap-x-2 text-[11.5px] text-muted-foreground">
                        <span className="font-mono">{hostOf(r.url)}</span>
                        {r.date && <span>{r.date.slice(0, 10)}</span>}
                      </p>
                      {r.snippet && <p className="mt-1.5 text-[12.5px] leading-relaxed text-secondary-foreground">{r.snippet}</p>}
                    </li>
                  ))}
                </ol>
              )}
            </>
          )}
        </div>
      </section>
    </div>
  );
}
