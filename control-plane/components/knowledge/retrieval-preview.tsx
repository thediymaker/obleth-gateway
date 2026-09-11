"use client";

import { useMemo, useState } from "react";
import { AlertTriangle, Search } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { DetailStat, EmptyState } from "@/components/dashboard-primitives";
import { formatCompact } from "@/lib/format";
import { injectionSkipReason } from "@/lib/knowledge-format";
import type { KnowledgeCollection, KnowledgeDocument, KnowledgeHit } from "@/lib/obleth";
import { cn } from "@/lib/utils";

// Lets an administrator run the same retrieval the knowledge boon runs at
// request time, against a chosen collection, so they can confirm a query
// actually surfaces the right chunks under the *current* settings before
// wiring the collection to a model. Talks to the live route rather than
// `lib/obleth` directly -- this is a client component (Correction 3).
export function RetrievalPreview({
  collection,
  documents,
  minScore,
  maxContextTokens,
}: {
  collection: KnowledgeCollection;
  documents: KnowledgeDocument[];
  minScore: number | null;
  maxContextTokens: number | null;
}) {
  const [query, setQuery] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hits, setHits] = useState<KnowledgeHit[] | null>(null);

  const titleByDocument = useMemo(
    () => new Map(documents.map((d) => [d.id, d.title])),
    [documents],
  );

  // The search endpoint embeds the query with `indexed_embedding_model` --
  // the embedder the collection's active generation was actually built with
  // -- and 400s when that's empty (no document has finished indexing yet).
  // Checking it here means the operator sees why before spending a round
  // trip on a query that can only fail.
  const neverIndexed = !collection.indexed_embedding_model;

  async function runSearch() {
    const trimmed = query.trim();
    if (!trimmed || neverIndexed) return;
    setPending(true);
    setError(null);
    try {
      const res = await fetch(`/api/live/knowledge/collections/${collection.id}/search`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ query: trimmed }),
      });
      const body = await res.json().catch(() => null);
      if (!res.ok) {
        setError((body && body.error) || `Search failed (HTTP ${res.status}).`);
        setHits(null);
        return;
      }
      setHits(body as KnowledgeHit[]);
    } catch {
      setError("Search failed. Check the network connection and try again.");
      setHits(null);
    } finally {
      setPending(false);
    }
  }

  return (
    <div className="space-y-3 rounded-lg border border-border/70 bg-background/30 p-4">
      <div>
        <p className="text-sm font-medium">Retrieval preview</p>
        <p className="mt-0.5 text-xs leading-relaxed text-muted-foreground">
          Runs the same retrieval a model with this collection attached would run, so a hit here
          predicts real behaviour rather than only showing similarity scores.
        </p>
      </div>

      <div className="grid grid-cols-2 gap-2 sm:max-w-xs">
        <DetailStat label="Min score" value={minScore == null ? "—" : minScore.toFixed(2)} />
        <DetailStat
          label="Max context tokens"
          value={maxContextTokens == null ? "—" : formatCompact(maxContextTokens)}
        />
      </div>

      {neverIndexed ? (
        <div className="flex items-start gap-2 rounded-md border border-amber-500/30 bg-amber-500/10 px-3 py-2.5 text-xs text-amber-200">
          <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          <p className="leading-relaxed">
            This collection has not been indexed yet. Upload a document and wait for it to reach
            &quot;ready&quot; before previewing retrieval.
          </p>
        </div>
      ) : (
        <div className="flex gap-2">
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void runSearch();
            }}
            placeholder="Ask a question this collection should answer"
            disabled={pending}
          />
          <Button type="button" onClick={() => void runSearch()} disabled={pending || !query.trim()}>
            <Search className="h-3.5 w-3.5" />
            {pending ? "Searching…" : "Search"}
          </Button>
        </div>
      )}

      {error && (
        <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive">
          {error}
        </p>
      )}

      {hits && hits.length === 0 && !error && (
        <EmptyState className="h-20">No chunks scored above the minimum score.</EmptyState>
      )}

      {hits && hits.length > 0 && (
        <ul className="space-y-2">
          {hits.map((hit) => (
            <li
              key={hit.chunk_id}
              className={cn(
                "rounded-md border px-3 py-2.5 text-xs leading-relaxed",
                hit.would_inject
                  ? "border-border/70 bg-card/45"
                  : "border-border/40 bg-background/20 text-muted-foreground/80 opacity-70",
              )}
            >
              <div className="mb-1 flex flex-wrap items-center gap-x-2 gap-y-1">
                <span className="font-mono text-[11px] tabular-nums">{hit.score.toFixed(2)}</span>
                <span className="text-muted-foreground/60">·</span>
                <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
                  {hit.token_count} tok
                </span>
                <span className="text-muted-foreground/60">·</span>
                <span className="truncate text-[11px] font-medium" title={titleByDocument.get(hit.document_id)}>
                  {titleByDocument.get(hit.document_id) ?? "(deleted document)"}
                </span>
                {!hit.would_inject && (
                  <Badge className="gap-1 border-amber-500/35 bg-amber-500/10 text-[10px] text-amber-300">
                    <AlertTriangle className="h-3 w-3" />
                    {injectionSkipReason(hit, minScore)}
                  </Badge>
                )}
              </div>
              <p>{hit.text}</p>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
