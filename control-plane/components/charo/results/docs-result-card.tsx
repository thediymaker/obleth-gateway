"use client";

import { Rail, MicroLabel } from "@/components/charo/rail";
import { cleanSnippet } from "@/lib/charo/docs/clean-snippet";
import type { DocsSearchResult, DocsSource } from "@/lib/charo/docs/types";

const DOCS_BASE = "https://obleth.com/docs";

function SourceRow({ source }: { source: DocsSource }) {
  return (
    <a href={`${DOCS_BASE}/${source.route}`} target="_blank" rel="noreferrer" className="group block">
      <span className="text-[13px] font-semibold text-violet-600 group-hover:underline dark:text-violet-300">{source.title}</span>
      <span className="text-[12px] text-muted-foreground"> › {source.heading}</span>
      <p className="mt-0.5 line-clamp-2 text-[12px] leading-[1.45] text-muted-foreground/90">
        {cleanSnippet(source.snippet)}
      </p>
    </a>
  );
}

export function DocsResultCard({ data }: { data: unknown }) {
  const r = (data ?? {}) as Partial<DocsSearchResult>;
  const sources = Array.isArray(r.sources) ? r.sources : [];
  return (
    <Rail>
      <MicroLabel className="mb-1.5">Sources · {sources.length}</MicroLabel>
      {sources.length === 0 ? (
        <p className="text-[12px] text-muted-foreground">No matching docs found.</p>
      ) : (
        <div className="space-y-2.5">
          {sources.map((s, i) => (
            <SourceRow key={i} source={s} />
          ))}
        </div>
      )}
    </Rail>
  );
}
