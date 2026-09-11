"use client";

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { AlertTriangle, BookOpen, ChevronRight, Loader2, Plus, RefreshCw, Trash2 } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { EmptyState } from "@/components/dashboard-primitives";
import { CollectionDetail } from "@/components/knowledge/collection-detail";
import { collectionStatus } from "@/lib/knowledge-format";
import { formatBytes, formatCompact } from "@/lib/format";
import type { KnowledgeCollection, KnowledgeDocument } from "@/lib/obleth";
import { cn } from "@/lib/utils";

type Status = ReturnType<typeof collectionStatus>;

const STATUS_STYLE: Record<Status, string> = {
  ready: "border-emerald-500/35 bg-emerald-500/10 text-emerald-300",
  indexing: "border-blue-500/30 bg-blue-500/10 text-blue-300",
  "needs re-index": "border-amber-500/35 bg-amber-500/10 text-amber-300",
  failed: "border-destructive/40 bg-destructive/10 text-destructive",
};

function StatusBadge({ status }: { status: Status }) {
  return (
    <Badge className={cn("gap-1 text-[10px]", STATUS_STYLE[status])}>
      {status === "indexing" && <Loader2 className="h-3 w-3 animate-spin" />}
      {(status === "failed" || status === "needs re-index") && <AlertTriangle className="h-3 w-3" />}
      {status}
    </Badge>
  );
}

export function CollectionList({
  collections,
  documentsByCollection,
  embeddingModels,
  maxUploadBytes,
  minScore,
  maxContextTokens,
}: {
  collections: KnowledgeCollection[];
  documentsByCollection: Record<string, KnowledgeDocument[]>;
  embeddingModels: string[];
  maxUploadBytes: number | null;
  minScore: number | null;
  maxContextTokens: number | null;
}) {
  const router = useRouter();
  const [pending, startTransition] = useTransition();
  const { confirm, confirmElement } = useConfirm();

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [embeddingModel, setEmbeddingModel] = useState("");
  const [rowBusy, setRowBusy] = useState<Set<string>>(new Set());
  const [message, setMessage] = useState<string | null>(null);

  function refresh() {
    router.refresh();
  }

  function withRowBusy(id: string, run: () => Promise<void>) {
    setRowBusy((s) => new Set(s).add(id));
    run()
      .catch((e) => {
        // A thrown/rejected run() (e.g. a network-level fetch failure) is a
        // distinct failure mode from a non-ok response; route it through the
        // same message banner rather than letting it surface only as an
        // unhandled rejection in the console.
        setMessage(e instanceof Error ? e.message : "Something went wrong. Check your connection and try again.");
      })
      .finally(() =>
        setRowBusy((s) => {
          const next = new Set(s);
          next.delete(id);
          return next;
        }),
      );
  }

  function openCreate() {
    setCreateError(null);
    setName("");
    setDescription("");
    setEmbeddingModel(embeddingModels[0] ?? "");
    setCreateOpen(true);
  }

  function submitCreate() {
    setCreateError(null);
    if (!name.trim() || !embeddingModel.trim()) {
      setCreateError("Name and embedding model are required.");
      return;
    }
    startTransition(async () => {
      const res = await fetch("/api/live/knowledge/collections", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: name.trim(),
          description: description.trim() || undefined,
          embedding_model: embeddingModel.trim(),
        }),
      });
      const body = await res.json().catch(() => null);
      if (!res.ok) {
        setCreateError((body && body.error) || `Create failed (HTTP ${res.status}).`);
        return;
      }
      setCreateOpen(false);
      refresh();
    });
  }

  async function handleDeleteCollection(collection: KnowledgeCollection) {
    const docCount = documentsByCollection[collection.id]?.length ?? 0;
    const ok = await confirm({
      title: "Delete collection",
      description: `Delete "${collection.name}"? This permanently deletes ${docCount} document${docCount === 1 ? "" : "s"} and all of their indexed chunks. This cannot be undone.`,
    });
    if (!ok) return;
    setMessage(null);
    withRowBusy(collection.id, async () => {
      const res = await fetch(`/api/live/knowledge/collections/${collection.id}`, { method: "DELETE" });
      if (res.ok) {
        if (selectedId === collection.id) setSelectedId(null);
        refresh();
        return;
      }
      const body = await res.json().catch(() => null);
      setMessage(
        (body && typeof body.error === "string" && body.error) ||
          `Failed to delete "${collection.name}" (HTTP ${res.status}).`,
      );
    });
  }

  async function handleReindexCollection(collection: KnowledgeCollection) {
    const ok = await confirm({
      title: "Reindex collection",
      description: `Every document in "${collection.name}" will be re-chunked and re-embedded with ${collection.embedding_model}. Until the old chunks are replaced, this collection transiently uses up to double its current storage. This cannot be undone.`,
      confirmLabel: "Reindex",
    });
    if (!ok) return;
    setMessage(null);
    withRowBusy(collection.id, async () => {
      const res = await fetch(`/api/live/knowledge/collections/${collection.id}/reindex`, {
        method: "POST",
      });
      if (res.ok) {
        refresh();
        return;
      }
      const body = await res.json().catch(() => null);
      setMessage(
        (body && typeof body.error === "string" && body.error) ||
          `Failed to reindex "${collection.name}" (HTTP ${res.status}).`,
      );
    });
  }

  const totalBytes = collections.reduce((sum, c) => sum + c.estimated_bytes, 0);
  const totalDocs = collections.reduce((sum, c) => sum + (documentsByCollection[c.id]?.length ?? 0), 0);
  const selected = collections.find((c) => c.id === selectedId) ?? null;

  if (collections.length === 0) {
    return (
      <>
        <div className="flex justify-end">
          <Button type="button" size="sm" variant="outline" onClick={openCreate} className="gap-1.5">
            <Plus className="h-3.5 w-3.5" />
            New collection
          </Button>
        </div>
        <div className="rounded-lg border border-dashed border-border/70 bg-background/30 px-6 py-10 text-center">
          <BookOpen className="mx-auto h-8 w-8 text-muted-foreground/70" />
          <p className="mt-3 text-sm font-medium">No collections yet</p>
          <p className="mx-auto mt-1 max-w-md text-sm leading-relaxed text-muted-foreground">
            A collection groups documents that are chunked and embedded together with one
            embedding model. Create one, upload documents, then attach it to a model so the
            model retrieves from it at request time.
          </p>
        </div>
        {createDialog()}
      </>
    );
  }

  return (
    <>
      {confirmElement}
      <div className="mb-3 flex items-center justify-between gap-3">
        <p className="text-xs text-muted-foreground">
          {collections.length} collection{collections.length === 1 ? "" : "s"} · {formatCompact(totalDocs)} document
          {totalDocs === 1 ? "" : "s"} · {formatBytes(totalBytes)} total
        </p>
        <Button type="button" size="sm" variant="outline" onClick={openCreate} className="gap-1.5">
          <Plus className="h-3.5 w-3.5" />
          New collection
        </Button>
      </div>

      {message && (
        <p className="mb-3 rounded-md border border-destructive/35 bg-destructive/10 px-3 py-2 text-xs text-destructive">
          {message}
        </p>
      )}

      <div className="overflow-hidden rounded-lg border border-border/70 bg-card/45">
        <div className="hidden grid-cols-[minmax(0,1fr)_10rem_6rem_6rem_8rem_2.75rem] border-b border-border/70 bg-background/35 px-4 py-2.5 text-xs font-medium text-muted-foreground md:grid">
          <div>Collection</div>
          <div>Embedding model</div>
          <div>Documents</div>
          <div>Chunks</div>
          <div>Status</div>
          <div />
        </div>
        <div className="divide-y divide-border/60">
          {collections.map((collection) => {
            const docs = documentsByCollection[collection.id] ?? [];
            const status = collectionStatus(collection, docs);
            const busy = rowBusy.has(collection.id);
            return (
              <div
                key={collection.id}
                className="group relative grid gap-3 px-4 py-4 md:grid-cols-[minmax(0,1fr)_10rem_6rem_6rem_8rem_auto] md:items-center"
              >
                <button
                  type="button"
                  onClick={() => setSelectedId(collection.id)}
                  className="absolute inset-0 cursor-pointer text-left transition-colors hover:bg-muted/20 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring focus-visible:ring-inset"
                  aria-label={`Open collection ${collection.name}`}
                />

                <div className="relative min-w-0 pointer-events-none">
                  <p className="truncate text-sm font-semibold" title={collection.name}>
                    {collection.name}
                  </p>
                  {collection.description && (
                    <p className="mt-0.5 line-clamp-1 text-xs text-muted-foreground">{collection.description}</p>
                  )}
                  <div className="mt-2 flex flex-wrap gap-1.5 md:hidden">
                    <Badge className="text-[10px]">{collection.embedding_model}</Badge>
                    <Badge className="text-[10px]">{formatCompact(docs.length)} docs</Badge>
                    <StatusBadge status={status} />
                  </div>
                </div>

                <div className="relative hidden min-w-0 pointer-events-none md:block">
                  <p className="truncate text-sm text-muted-foreground" title={collection.embedding_model}>
                    {collection.embedding_model}
                  </p>
                </div>

                <div className="relative hidden pointer-events-none md:block">
                  <p className="text-sm tabular-nums">{formatCompact(docs.length)}</p>
                </div>

                <div className="relative hidden pointer-events-none md:block">
                  <p className="text-sm tabular-nums text-muted-foreground">{formatCompact(collection.chunk_count)}</p>
                  <p className="text-[11px] text-muted-foreground/75">{formatBytes(collection.estimated_bytes)}</p>
                </div>

                <div className="relative hidden pointer-events-none md:block">
                  <StatusBadge status={status} />
                </div>

                <div className="relative hidden items-center justify-end gap-1 md:flex">
                  {status === "needs re-index" && (
                    <button
                      type="button"
                      title="Reindex collection"
                      onClick={() => void handleReindexCollection(collection)}
                      disabled={busy}
                      className="flex h-7 w-7 items-center justify-center rounded-md border border-amber-500/40 bg-background/45 text-amber-300 transition-colors hover:border-amber-500 disabled:opacity-50"
                    >
                      <RefreshCw className={cn("h-3.5 w-3.5", busy && "animate-spin")} />
                    </button>
                  )}
                  <button
                    type="button"
                    title="Delete collection"
                    onClick={() => void handleDeleteCollection(collection)}
                    disabled={busy}
                    className="flex h-7 w-7 items-center justify-center rounded-md border border-destructive/40 bg-background/45 text-destructive/70 transition-colors hover:border-destructive hover:text-destructive disabled:opacity-50"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                  <span className="flex h-7 w-7 items-center justify-center rounded-md border border-border/70 bg-background/45 text-muted-foreground transition-colors group-hover:border-border group-hover:text-foreground pointer-events-none">
                    <ChevronRight className="h-4 w-4" />
                  </span>
                </div>
              </div>
            );
          })}
        </div>
      </div>

      <Dialog open={selected !== null} onOpenChange={(open) => !open && setSelectedId(null)}>
        <DialogContent className="grid max-h-[80vh] w-[min(960px,calc(100vw-2rem))] max-w-none grid-rows-[auto_minmax(0,1fr)] gap-0 overflow-hidden p-0">
          {selected && (
            <>
              <DialogHeader className="border-b border-border/70 bg-background/35 px-6 py-4 pr-12 text-left">
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div className="min-w-0">
                    <p className="mb-1 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
                      Collection
                    </p>
                    <DialogTitle className="truncate text-lg">{selected.name}</DialogTitle>
                    {selected.description && (
                      <DialogDescription className="mt-1 max-w-2xl">{selected.description}</DialogDescription>
                    )}
                  </div>
                </div>
              </DialogHeader>
              <div className="min-h-0 overflow-y-auto px-5 py-4 sm:px-6">
                <CollectionDetail
                  collection={selected}
                  initialDocuments={documentsByCollection[selected.id] ?? []}
                  maxUploadBytes={maxUploadBytes}
                  minScore={minScore}
                  maxContextTokens={maxContextTokens}
                  onClose={() => setSelectedId(null)}
                  onChanged={refresh}
                />
              </div>
            </>
          )}
        </DialogContent>
      </Dialog>

      {createDialog()}
    </>
  );

  function createDialog() {
    return (
      <Dialog open={createOpen} onOpenChange={setCreateOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>New collection</DialogTitle>
            <DialogDescription>
              Documents added to this collection are chunked and embedded with the model you
              choose here.
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-3">
            <div className="space-y-1.5">
              <Label htmlFor="kb-name">Name</Label>
              <Input
                id="kb-name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="e.g. Employee handbook"
                disabled={pending}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="kb-description">Description (optional)</Label>
              <Input
                id="kb-description"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder="What this collection is for"
                disabled={pending}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="kb-embedding-model">Embedding model</Label>
              <Input
                id="kb-embedding-model"
                value={embeddingModel}
                onChange={(e) => setEmbeddingModel(e.target.value)}
                placeholder="e.g. bge-small-en"
                list="kb-embedding-models"
                disabled={pending}
              />
              {embeddingModels.length > 0 && (
                <datalist id="kb-embedding-models">
                  {embeddingModels.map((m) => (
                    <option key={m} value={m} />
                  ))}
                </datalist>
              )}
              {embeddingModels.length === 0 && (
                <p className="text-xs text-muted-foreground">
                  No embedding-type models are registered yet; enter the model name to use once one
                  is added.
                </p>
              )}
            </div>
            {createError && (
              <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
                {createError}
              </p>
            )}
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setCreateOpen(false)} disabled={pending}>
              Cancel
            </Button>
            <Button type="button" onClick={submitCreate} disabled={pending}>
              {pending ? "Creating…" : "Create collection"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    );
  }
}
