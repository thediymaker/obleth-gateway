"use client";

import { useCallback, useEffect, useMemo, useState, useTransition } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { MoreHorizontal, Search } from "lucide-react";
import { attachCollectionAction } from "@/app/actions";
import { SettingsForm, type SaveResult } from "@/components/access/settings-form";
import { SettingsCard } from "@/components/access/ui";
import { ChangesList } from "@/components/deployments/detail-ui";
import { DocumentUpload } from "@/components/knowledge/document-upload";
import { StatusMark } from "@/components/knowledge/knowledge-list";
import { SelectField, Setting, TextField } from "@/components/models/fields";
import { Notice, Tile } from "@/components/models/ui";
import { Pill } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Select } from "@/components/ui/select";
import { formatBytes } from "@/lib/format";
import { COLLECTION_STATUS_LABEL, collectionGlyph, collectionHref, collectionStatus, injectionSkipReason, retrievalUse } from "@/lib/knowledge-format";
import { modelHref } from "@/lib/models-model";
import type { AuditEntry, DailyStatsView, KnowledgeCollection, KnowledgeDocument, KnowledgeHit, KnowledgeSettingsView, ModelRoute } from "@/lib/obleth";
import { compact } from "@/lib/overview-model";
import { cn, getJson } from "@/lib/utils";

const DOC_COLS = "grid-cols-[minmax(0,1fr)_90px_70px_minmax(110px,160px)_150px]";

function docMark(d: KnowledgeDocument) {
  if (d.status === "failed") return <StatusMark label="Failed" glyph="attention" />;
  if (d.status === "ready") return <StatusMark label="Indexed" glyph="on" />;
  return <StatusMark label={d.status === "pending" ? "Queued" : d.reindex_requested ? "Reindex queued" : "Indexing"} glyph="half" />;
}

async function call(url: string, method: string, body?: unknown): Promise<string | null> {
  try {
    const res = await fetch(url, { method, headers: body ? { "Content-Type": "application/json" } : undefined, body: body ? JSON.stringify(body) : undefined });
    if (res.ok) return null;
    const b = await res.json().catch(() => null);
    return (b && typeof b.error === "string" && b.error) || `That didn't work (HTTP ${res.status}).`;
  } catch (e) {
    return e instanceof Error ? e.message : String(e);
  }
}

function SearchPanel({ collection, documents, settings }: { collection: KnowledgeCollection; documents: KnowledgeDocument[]; settings: KnowledgeSettingsView | null }) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<KnowledgeHit[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();
  const titles = useMemo(() => new Map(documents.map((d) => [d.id, d.title])), [documents]);
  const neverIndexed = !collection.indexed_embedding_model;
  function run() {
    const q = query.trim();
    if (!q || neverIndexed) return;
    setError(null);
    start(async () => {
      try {
        const res = await fetch(`/api/live/knowledge/collections/${collection.id}/search`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ query: q }) });
        const body = await res.json().catch(() => null);
        if (!res.ok) { setHits(null); setError((body && body.error) || `The search failed (HTTP ${res.status}).`); return; }
        setHits(body as KnowledgeHit[]);
      } catch {
        setHits(null);
        setError("The search failed. Check the connection and try again.");
      }
    });
  }
  return (
    <section id="search" aria-label="Try a search" className="scroll-mt-24 rounded-xl border border-border bg-card px-[18px] pb-3 pt-4">
      <h2 className="text-sm font-semibold">Try a search</h2>
      <p className="mt-0.5 text-xs text-muted-foreground">
        What a model would be handed for a question, with the current retrieval settings{settings ? `: top ${settings.top_k}, score at least ${settings.min_score}, up to ${settings.max_context_tokens.toLocaleString()} tokens` : ""}.
      </p>
      {neverIndexed ? (
        <p className="mt-3 text-[12.5px] text-secondary-foreground">Nothing is indexed yet. Upload a document and wait for it to show Indexed.</p>
      ) : (
        <form className="my-3 flex gap-2" onSubmit={(e) => { e.preventDefault(); run(); }}>
          <label className="flex h-9 flex-1 items-center gap-2 rounded-lg border border-border bg-background px-3 text-[13px] focus-within:border-muted-foreground">
            <Search className="h-4 w-4 shrink-0 text-muted-foreground" aria-hidden />
            <input value={query} onChange={(e) => setQuery(e.target.value)} aria-label="A question this collection should answer" placeholder="A question this collection should answer" className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground" />
          </label>
          <Button type="submit" size="sm" className="h-9" disabled={pending || !query.trim()}>{pending ? "Searching…" : "Search"}</Button>
        </form>
      )}
      {error && <Notice strong>{error}</Notice>}
      {hits && hits.length === 0 && !error && <p className="py-3 text-[12.5px] text-muted-foreground">Nothing scored above the minimum score.</p>}
      {hits?.map((h) => (
        <div key={h.chunk_id} className={cn("flex flex-col gap-1.5 border-t border-border py-3", !h.would_inject && "opacity-55")}>
          <div className="flex flex-wrap justify-between gap-2 text-xs">
            <span className="font-mono">{titles.get(h.document_id) ?? "a deleted document"}</span>
            <span className="font-mono text-secondary-foreground">{h.score.toFixed(2)} · {h.token_count} tokens · {h.would_inject ? "included" : `left out: ${injectionSkipReason(h, settings?.min_score ?? null)}`}</span>
          </div>
          <p className="line-clamp-4 whitespace-pre-line text-[12.5px] leading-relaxed text-secondary-foreground">{h.text}</p>
        </div>
      ))}
    </section>
  );
}

export function CollectionPage({
  collection,
  initialDocuments,
  settings,
  models,
  usedBy,
  stats,
  changes,
}: {
  collection: KnowledgeCollection;
  initialDocuments: KnowledgeDocument[];
  settings: KnowledgeSettingsView | null;
  models: ModelRoute[];
  usedBy: string[];
  stats: DailyStatsView | null;
  changes: AuditEntry[];
}) {
  const router = useRouter();
  const queryClient = useQueryClient();
  const { confirm, confirmElement } = useConfirm();
  const [pending, start] = useTransition();
  const [notice, setNotice] = useState<{ text: string; strong?: boolean } | null>(null);
  const [attach, setAttach] = useState("");
  const docsKey = ["collection-docs", collection.id];
  const docsQuery = useQuery({
    queryKey: docsKey,
    queryFn: () => getJson<KnowledgeDocument[]>(`/api/live/knowledge/collections/${collection.id}/documents`),
    initialData: initialDocuments,
    refetchInterval: (q) => ((q.state.data ?? []).some((d) => d.status === "pending" || d.status === "indexing") ? 4000 : false),
  });
  const docs = useMemo(() => [...(docsQuery.data ?? [])].sort((a, b) => Number(b.status === "failed") - Number(a.status === "failed") || a.title.localeCompare(b.title)), [docsQuery.data]);
  const status = collectionStatus(collection, docs);
  const failed = docs.filter((d) => d.status === "failed").length;
  const text = docs.reduce((n, d) => n + d.byte_size, 0);
  const use = retrievalUse(stats?.items.find((i) => i.id === collection.id));
  const chat = models.filter((m) => m.model_type === "chat").sort((a, b) => a.model_name.localeCompare(b.model_name));
  const embedders = [...new Set([collection.embedding_model, ...models.filter((m) => m.model_type === "embedding").map((m) => m.model_name)])].filter(Boolean).sort();
  const [embedder, setEmbedder] = useState(collection.embedding_model);

  useEffect(() => {
    // A document that finished indexing changes the collection's chunk count; refresh the server data once.
    if (!docsQuery.isFetching && docs.every((d) => d.status === "ready" || d.status === "failed")) router.refresh();
  }, [docsQuery.dataUpdatedAt]); // eslint-disable-line react-hooks/exhaustive-deps

  const refreshDocs = () => queryClient.invalidateQueries({ queryKey: docsKey });

  function act(task: () => Promise<string | null>, done: string) {
    start(async () => {
      const err = await task();
      setNotice(err ? { text: err, strong: true } : { text: done });
      await refreshDocs();
      router.refresh();
    });
  }

  function reindexAll() {
    start(async () => {
      const ok = await confirm({ title: `Reindex ${collection.name}?`, description: `Every document is split and embedded again with ${collection.embedding_model}. Retrieval keeps using the current chunks until the new ones are ready.`, confirmLabel: "Reindex" });
      if (!ok) return;
      const err = await call(`/api/live/knowledge/collections/${collection.id}/reindex`, "POST");
      setNotice(err ? { text: err, strong: true } : { text: "Reindexing. Documents show Indexing until they're done." });
      await refreshDocs();
    });
  }

  function remove() {
    start(async () => {
      const ok = await confirm({ title: `Delete ${collection.name}?`, description: `Its ${docs.length} documents and ${collection.chunk_count} chunks are deleted${usedBy.length ? `, and ${usedBy.join(", ")} stop${usedBy.length === 1 ? "s" : ""} retrieving from it` : ""}. This cannot be undone.`, confirmLabel: "Delete" });
      if (!ok) return;
      const err = await call(`/api/live/knowledge/collections/${collection.id}`, "DELETE");
      if (err) return setNotice({ text: err, strong: true });
      router.push("/knowledge");
    });
  }

  function deleteDoc(d: KnowledgeDocument) {
    start(async () => {
      const ok = await confirm({ title: `Delete ${d.title}?`, description: `Its ${d.chunk_count} chunks leave the collection at once.`, confirmLabel: "Delete" });
      if (!ok) return;
      const err = await call(`/api/live/knowledge/documents/${d.id}`, "DELETE");
      setNotice(err ? { text: err, strong: true } : { text: `Deleted ${d.title}.` });
      await refreshDocs();
      router.refresh();
    });
  }

  function link(modelId: string, on: boolean, name: string) {
    start(async () => {
      const res = await attachCollectionAction(modelId, collection.id, on);
      setNotice(res.ok ? { text: on ? `${name} now retrieves from ${collection.name}.${settings?.enabled ? "" : " Retrieval is off, so turn it on on the Knowledge page."}` : `${name} no longer retrieves from it.` } : { text: res.error, strong: true });
      setAttach("");
      router.refresh();
    });
  }

  const save = useCallback(async (data: FormData): Promise<SaveResult> => {
    const tokens = Number(data.get("chunk_tokens"));
    const overlap = Number(data.get("chunk_overlap_tokens"));
    if (!Number.isInteger(tokens) || tokens < 32) return { ok: false, error: "Chunks need at least 32 tokens.", saved: [] };
    if (!Number.isInteger(overlap) || overlap < 0 || overlap >= tokens) return { ok: false, error: "Overlap must be less than the chunk size.", saved: [] };
    const name = String(data.get("name") ?? "").trim();
    if (!name) return { ok: false, error: "Name it.", saved: [] };
    const err = await call(`/api/live/knowledge/collections/${collection.id}`, "PUT", {
      name,
      description: String(data.get("description") ?? "").trim(),
      embedding_model: String(data.get("embedding_model") ?? "").trim(),
      chunk_tokens: tokens,
      chunk_overlap_tokens: overlap,
    });
    if (err) return { ok: false, error: err, saved: [] };
    if (name !== collection.name) router.replace(collectionHref(name));
    else router.refresh();
    return { ok: true };
  }, [collection.id, collection.name, router]);
  const sectionOf = useCallback((n: string) => (["name", "description", "embedding_model", "chunk_tokens", "chunk_overlap_tokens"].includes(n) ? "collection" : null), []);
  const labelOf = useCallback((n: string) => ({ name: "Name", description: "Description", embedding_model: "Embedding model", chunk_tokens: "Chunk size", chunk_overlap_tokens: "Overlap" })[n] ?? n, []);

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-5">
      {confirmElement}
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          <p className="text-[12.5px] text-muted-foreground"><Link href="/knowledge" className="text-secondary-foreground hover:text-foreground">Knowledge</Link> / Collection</p>
          <h1 className="truncate text-[26px] font-semibold tracking-tight">{collection.name}</h1>
          <div className="flex flex-wrap items-center gap-2">
            <StatusMark label={docs.length ? COLLECTION_STATUS_LABEL[status] : "Empty"} glyph={collectionGlyph(status, docs.length)} />
            <Pill className="font-mono text-[11px]">{collection.embedding_model}{collection.embedding_dim ? ` · ${collection.embedding_dim} dims` : ""}</Pill>
            {collection.description && <span className="text-[12.5px] text-muted-foreground">{collection.description}</span>}
          </div>
        </div>
        <div className="flex gap-2">
          <Button type="button" variant="outline" size="sm" className="h-9" disabled={pending || docs.length === 0} onClick={reindexAll}>Reindex</Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild><Button type="button" variant="outline" size="icon" className="h-9 w-9" aria-label="More actions"><MoreHorizontal className="h-4 w-4" /></Button></DropdownMenuTrigger>
            <DropdownMenuContent align="end"><DropdownMenuItem onSelect={remove}>Delete collection…</DropdownMenuItem></DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>

      {collection.needs_reindex && <Notice strong>Its settings changed since it was indexed, so retrieval still uses the old chunks{collection.indexed_embedding_model && collection.indexed_embedding_model !== collection.embedding_model ? ` (embedded with ${collection.indexed_embedding_model})` : ""}. Reindex to use the new ones.</Notice>}
      {settings && !settings.enabled && <Notice>Retrieval is off for every model. <Link href="/knowledge?tab=retrieval" className="underline underline-offset-2">Turn it on</Link> for attached models to use this collection.</Notice>}
      {notice && <Notice onDismiss={() => setNotice(null)} strong={notice.strong}>{notice.text}</Notice>}

      <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
        <Tile label="Documents" value={docs.length} detail={failed ? `${failed} failed` : `${docs.filter((d) => d.status === "ready").length} indexed`} emphasis={failed > 0} />
        <Tile label="Chunks" value={compact(collection.chunk_count)} detail={docs.length ? `about ${Math.max(1, Math.round(collection.chunk_count / docs.length))} per document` : null} />
        <Tile label="Text" value={formatBytes(text)} detail={`${formatBytes(collection.estimated_bytes)} with vectors`} />
        <Tile label="Searches · 30 days" value={compact(use.searches)} detail={use.searches ? `${Math.round((use.hits / use.searches) * 100)}% found something · ${compact(use.chunks)} chunks used` : usedBy.length ? "none yet" : "attach it to a model below"} />
      </div>

      <section id="documents" aria-label="Documents" className="scroll-mt-24 rounded-xl border border-border bg-card">
        <header className="px-[18px] pb-3 pt-4">
          <h2 className="text-sm font-semibold">Documents <span className="font-normal text-muted-foreground">· {docs.length}</span></h2>
          <p className="mt-0.5 text-xs text-muted-foreground">Chunks of {collection.chunk_tokens} tokens with {collection.chunk_overlap_tokens} overlapping.</p>
        </header>
        <div className="px-[18px] pb-3">
          <DocumentUpload collectionId={collection.id} maxUploadBytes={settings?.max_upload_bytes ?? null} onUploaded={() => void refreshDocs()} onSettled={() => { void refreshDocs(); router.refresh(); }} />
        </div>
        {docs.length > 0 && (
          <div className="overflow-x-auto">
            <div className="min-w-[720px]">
              <div className={cn("grid items-center gap-3 px-[18px] py-2 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground", DOC_COLS)}><span>Document</span><span className="text-right">Size</span><span className="text-right">Chunks</span><span>Status</span><span /></div>
              {docs.map((d) => (
                <div key={d.id} className={cn("grid min-h-[44px] items-center gap-3 border-t border-border px-[18px] py-1.5 text-[13px]", DOC_COLS)}>
                  <span className="flex min-w-0 flex-col"><span className="truncate font-mono text-[12.5px]">{d.title}</span>{d.error && <span className="truncate text-[11.5px] text-foreground" title={d.error}>{d.error}</span>}</span>
                  <span className="text-right font-mono text-[12px]">{formatBytes(d.byte_size)}</span>
                  <span className="text-right font-mono text-[12px]">{d.chunk_count}</span>
                  <span>{docMark(d)}</span>
                  <span className="flex justify-end gap-3 text-[12.5px]">
                    <button type="button" disabled={pending} onClick={() => act(() => call(`/api/live/knowledge/documents/${d.id}/reindex`, "POST"), `Reindexing ${d.title}.`)} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground disabled:opacity-50">Reindex</button>
                    <button type="button" disabled={pending} onClick={() => deleteDoc(d)} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground disabled:opacity-50">Delete</button>
                  </span>
                </div>
              ))}
            </div>
          </div>
        )}
      </section>

      <SearchPanel collection={collection} documents={docs} settings={settings} />

      <section id="used-by" aria-label="Used by" className="scroll-mt-24 rounded-xl border border-border bg-card px-[18px] pb-3 pt-4">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <h2 className="text-sm font-semibold">Used by</h2>
            <p className="mt-0.5 text-xs text-muted-foreground">Chat models that retrieve from it. Attaching gives a model its Knowledge boon.</p>
          </div>
          <div className="flex items-center gap-2">
            <div className="w-64"><Select aria-label="Model to attach" value={attach} onValueChange={setAttach} searchPlaceholder="Find a model" className="h-9 text-[13px]" options={[{ value: "", label: "Pick a chat model" }, ...chat.filter((m) => !usedBy.includes(m.model_name)).map((m) => ({ value: m.id, label: m.model_name }))]} /></div>
            <Button type="button" size="sm" className="h-9" disabled={pending || !attach} onClick={() => link(attach, true, chat.find((m) => m.id === attach)?.model_name ?? "The model")}>Attach</Button>
          </div>
        </div>
        {usedBy.length === 0 ? (
          <p className="pt-3 text-[12.5px] text-muted-foreground">No model yet.</p>
        ) : (
          usedBy.map((name) => {
            const m = models.find((x) => x.model_name === name);
            return (
              <div key={name} className="flex items-center justify-between gap-3 border-t border-border py-2 text-[13px] first:mt-2">
                <Link href={modelHref(name, "knowledge")} className="font-mono text-[12.5px] hover:underline">{name}</Link>
                {m && <button type="button" disabled={pending} onClick={() => link(m.id, false, name)} className="text-[12.5px] text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">Detach</button>}
              </div>
            );
          })
        )}
      </section>

      <SettingsForm id={collection.id} sectionOf={sectionOf} labelOf={labelOf} save={save} className="flex flex-col gap-4" ariaLabel={`${collection.name} settings`}>
        <SettingsCard id="settings" title="Settings">
          <Setting label="Name · description" fields={["name", "description"]}>
            <div className="flex flex-wrap gap-2">
              <TextField name="name" label="Name" required defaultValue={collection.name} className="w-64" />
              <TextField name="description" label="Description" defaultValue={collection.description} placeholder="Optional" className="w-80" />
            </div>
          </Setting>
          <Setting label="Embedding model" hint="Changing it re-embeds every chunk." fields={["embedding_model"]} was={{ field: "embedding_model" }}>
            <div className="w-72"><SelectField name="embedding_model" label="Embedding model" value={embedder} onChange={setEmbedder} options={embedders.map((m) => ({ value: m, label: m }))} /></div>
          </Setting>
          <Setting label="Chunks" hint="Tokens per chunk and how many overlap. A change needs a reindex." fields={["chunk_tokens", "chunk_overlap_tokens"]}>
            <div className="flex flex-wrap items-center gap-2">
              <TextField name="chunk_tokens" label="Chunk size" inputMode="numeric" defaultValue={collection.chunk_tokens} mono className="w-24" />
              <span className="text-xs text-muted-foreground">tokens, overlap</span>
              <TextField name="chunk_overlap_tokens" label="Overlap" inputMode="numeric" defaultValue={collection.chunk_overlap_tokens} mono className="w-20" />
            </div>
          </Setting>
        </SettingsCard>
      </SettingsForm>

      <ChangesList changes={changes} />
    </div>
  );
}
