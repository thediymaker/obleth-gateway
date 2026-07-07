"use client";

import type { DocsSearchResult, DocsSource } from "@/lib/charo/docs/types";

const DOCS_BASE = "https://obleth.com/docs";

function SourceRow({ source }: { source: DocsSource }) {
  return (
    <a
      href={`${DOCS_BASE}/${source.route}`}
      target="_blank"
      rel="noreferrer"
      className="block border-b border-border py-2 last:border-0 hover:bg-muted/40"
    >
      <div className="flex items-baseline gap-2">
        <span className="text-sm font-medium">{source.title}</span>
        <span className="truncate text-xs text-muted-foreground">· {source.heading}</span>
      </div>
      <p className="mt-0.5 line-clamp-2 text-xs text-muted-foreground">{source.snippet}</p>
    </a>
  );
}

export function DocsResultCard({ data }: { data: unknown }) {
  const r = (data ?? {}) as Partial<DocsSearchResult>;
  const sources = Array.isArray(r.sources) ? r.sources : [];
  return (
    <div className="w-full rounded-lg border border-border bg-card p-3">
      <div className="mb-1 flex items-center justify-between gap-2">
        <div className="truncate text-sm font-semibold">Documentation</div>
        <span className="rounded-full border border-border px-2 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground">
          docs
        </span>
      </div>
      {sources.length === 0 ? (
        <p className="py-2 text-sm text-muted-foreground">No matching docs found.</p>
      ) : (
        <div>
          {sources.map((s, i) => (
            <SourceRow key={i} source={s} />
          ))}
        </div>
      )}
    </div>
  );
}
