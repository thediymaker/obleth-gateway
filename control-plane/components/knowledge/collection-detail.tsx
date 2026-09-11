"use client";

import { useEffect, useRef, useState } from "react";
import { AlertTriangle, Loader2, Pencil, RefreshCw, Trash2 } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { DetailStat, EmptyState } from "@/components/dashboard-primitives";
import { DocumentUpload } from "@/components/knowledge/document-upload";
import { RetrievalPreview } from "@/components/knowledge/retrieval-preview";
import { formatBytes, formatCompact } from "@/lib/format";
import type { KnowledgeCollection, KnowledgeDocument } from "@/lib/obleth";
import { cn } from "@/lib/utils";

const POLL_MS = 4000;

function statusBadge(doc: KnowledgeDocument) {
  switch (doc.status) {
    case "ready":
      return (
        <Badge className="border-emerald-500/35 bg-emerald-500/10 text-[10px] text-emerald-300">
          Ready
        </Badge>
      );
    case "failed":
      return (
        <Badge className="gap-1 border-destructive/40 bg-destructive/10 text-[10px] text-destructive">
          <AlertTriangle className="h-3 w-3" />
          Failed
        </Badge>
      );
    default:
      return (
        <Badge className="gap-1 border-blue-500/30 bg-blue-500/10 text-[10px] text-blue-300">
          <Loader2 className="h-3 w-3 animate-spin" />
          {doc.status === "indexing" ? "Indexing" : "Pending"}
        </Badge>
      );
  }
}

export function CollectionDetail({
  collection,
  initialDocuments,
  maxUploadBytes,
  minScore,
  maxContextTokens,
  onClose,
  onChanged,
}: {
  collection: KnowledgeCollection;
  initialDocuments: KnowledgeDocument[];
  maxUploadBytes: number | null;
  minScore: number | null;
  maxContextTokens: number | null;
  onClose: () => void;
  onChanged: () => void;
}) {
  const [documents, setDocuments] = useState(initialDocuments);
  const [busyIds, setBusyIds] = useState<Set<string>>(new Set());
  const [reindexPending, setReindexPending] = useState(false);
  const [message, setMessage] = useState<{ text: string; showReindex?: boolean } | null>(null);
  const { confirm, confirmElement } = useConfirm();
  const collectionIdRef = useRef(collection.id);

  const [editing, setEditing] = useState(false);
  const [editSaving, setEditSaving] = useState(false);
  const [editError, setEditError] = useState<string | null>(null);
  const [editName, setEditName] = useState(collection.name);
  const [editDescription, setEditDescription] = useState(collection.description);
  const [editEmbeddingModel, setEditEmbeddingModel] = useState(collection.embedding_model);
  const [editChunkTokens, setEditChunkTokens] = useState(String(collection.chunk_tokens));
  const [editChunkOverlapTokens, setEditChunkOverlapTokens] = useState(
    String(collection.chunk_overlap_tokens),
  );

  // A fresh selection resets the local document list, and any in-progress
  // edit buffer, to whatever the server component last loaded for it.
  useEffect(() => {
    collectionIdRef.current = collection.id;
    setDocuments(initialDocuments);
    setEditing(false);
    setEditError(null);
    setEditName(collection.name);
    setEditDescription(collection.description);
    setEditEmbeddingModel(collection.embedding_model);
    setEditChunkTokens(String(collection.chunk_tokens));
    setEditChunkOverlapTokens(String(collection.chunk_overlap_tokens));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [collection.id]);

  function openEdit() {
    setEditError(null);
    setEditName(collection.name);
    setEditDescription(collection.description);
    setEditEmbeddingModel(collection.embedding_model);
    setEditChunkTokens(String(collection.chunk_tokens));
    setEditChunkOverlapTokens(String(collection.chunk_overlap_tokens));
    setEditing(true);
  }

  async function handleSaveEdit() {
    setEditError(null);
    const name = editName.trim();
    const embeddingModel = editEmbeddingModel.trim();
    const chunkTokens = Number(editChunkTokens);
    const chunkOverlapTokens = Number(editChunkOverlapTokens);
    if (!name) {
      setEditError("Name is required.");
      return;
    }
    if (!embeddingModel) {
      setEditError("Embedding model is required.");
      return;
    }
    if (!Number.isFinite(chunkTokens) || chunkTokens <= 0) {
      setEditError("Chunk tokens must be a positive number.");
      return;
    }
    if (!Number.isFinite(chunkOverlapTokens) || chunkOverlapTokens < 0) {
      setEditError("Chunk overlap tokens must be zero or a positive number.");
      return;
    }
    // Mirrors the server's own check constraint (obleth-admin rejects the
    // same condition with a 400) so the operator gets this message instead
    // of a round trip to learn it. The server remains authoritative.
    if (chunkOverlapTokens >= chunkTokens) {
      setEditError("Chunk overlap tokens must be less than chunk tokens.");
      return;
    }
    const chunkingChanged =
      embeddingModel !== collection.embedding_model ||
      chunkTokens !== collection.chunk_tokens ||
      chunkOverlapTokens !== collection.chunk_overlap_tokens;
    setEditSaving(true);
    try {
      const res = await fetch(`/api/live/knowledge/collections/${collection.id}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name,
          description: editDescription.trim(),
          embedding_model: embeddingModel,
          chunk_tokens: chunkTokens,
          chunk_overlap_tokens: chunkOverlapTokens,
        }),
      });
      const body = await res.json().catch(() => null);
      if (!res.ok) {
        setEditError((body && body.error) || `Save failed (HTTP ${res.status}).`);
        return;
      }
      setEditing(false);
      setMessage(
        chunkingChanged
          ? {
              text: "Collection saved. Existing chunks were built with the previous settings and won't change until you reindex.",
              showReindex: true,
            }
          : { text: "Collection saved." },
      );
      onChanged();
    } catch {
      setEditError("Save failed. Check the network connection and try again.");
    } finally {
      setEditSaving(false);
    }
  }

  async function refreshDocuments() {
    try {
      const res = await fetch(`/api/live/knowledge/collections/${collection.id}/documents`);
      if (!res.ok) return;
      const fresh = (await res.json()) as KnowledgeDocument[];
      // This fetch can resolve after the dialog/component has unmounted. That's
      // deliberately left unguarded: under React 18+, calling setDocuments on an
      // unmounted component is a harmless no-op (no warning, no leak), and the
      // polling interval that could keep this fetch firing is already cleared on
      // unmount below — so there's nothing here for a mounted-ref to protect.
      if (collectionIdRef.current === collection.id) setDocuments(fresh);
    } catch {
      // Transient fetch failure; the next poll tick or manual action retries.
    }
  }

  // Poll while any document is still being worked on, so status flips from
  // pending/indexing to ready or failed without the operator refreshing.
  useEffect(() => {
    const outstanding = documents.some((d) => d.status === "pending" || d.status === "indexing");
    if (!outstanding) return;
    const id = window.setInterval(() => void refreshDocuments(), POLL_MS);
    return () => window.clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [documents, collection.id]);

  function withBusy(id: string, run: () => Promise<void>) {
    setBusyIds((s) => new Set(s).add(id));
    run()
      .catch((e) => {
        // A thrown/rejected run() (e.g. a network-level fetch failure) is a
        // distinct failure mode from a non-ok response; route it through the
        // same message banner rather than letting it surface only as an
        // unhandled rejection in the console.
        setMessage({
          text: e instanceof Error ? e.message : "Something went wrong. Check your connection and try again.",
        });
      })
      .finally(() =>
        setBusyIds((s) => {
          const next = new Set(s);
          next.delete(id);
          return next;
        }),
      );
  }

  async function handleDeleteDocument(doc: KnowledgeDocument) {
    const ok = await confirm({
      title: "Delete document",
      description: `Delete "${doc.title}"? Its indexed chunks are removed immediately and this cannot be undone.`,
    });
    if (!ok) return;
    withBusy(doc.id, async () => {
      const res = await fetch(`/api/live/knowledge/documents/${doc.id}`, { method: "DELETE" });
      if (res.ok) {
        setDocuments((prev) => prev.filter((d) => d.id !== doc.id));
        onChanged();
      } else {
        setMessage({ text: `Failed to delete "${doc.title}".` });
      }
    });
  }

  async function handleReindexDocument(doc: KnowledgeDocument) {
    withBusy(doc.id, async () => {
      const res = await fetch(`/api/live/knowledge/documents/${doc.id}/reindex`, { method: "POST" });
      if (res.ok) {
        const updated = (await res.json()) as KnowledgeDocument;
        setDocuments((prev) => prev.map((d) => (d.id === doc.id ? updated : d)));
        onChanged();
      } else {
        setMessage({ text: `Failed to queue "${doc.title}" for reindexing.` });
      }
    });
  }

  async function handleReindexCollection() {
    const ok = await confirm({
      title: "Reindex collection",
      description: `Every document in "${collection.name}" will be re-chunked and re-embedded with ${collection.embedding_model}. Until the old chunks are replaced, this collection transiently uses up to double its current storage. This cannot be undone.`,
      confirmLabel: "Reindex",
    });
    if (!ok) return;
    setReindexPending(true);
    setMessage(null);
    try {
      const res = await fetch(`/api/live/knowledge/collections/${collection.id}/reindex`, {
        method: "POST",
      });
      if (res.ok) {
        const result = (await res.json()) as { documents_requeued: number };
        setMessage({ text: `${result.documents_requeued} document(s) queued for reindexing.` });
        await refreshDocuments();
        onChanged();
      } else {
        setMessage({ text: "Failed to start reindexing." });
      }
    } finally {
      setReindexPending(false);
    }
  }

  const failedDocs = documents.filter((d) => d.status === "failed");
  const staleEmbedder = collection.needs_reindex;
  const sortedDocuments = [...documents].sort((a, b) => {
    const rank = (d: KnowledgeDocument) => (d.status === "failed" ? 0 : d.status === "ready" ? 2 : 1);
    return rank(a) - rank(b);
  });

  return (
    <div className="space-y-4">
      {confirmElement}

      <div className="flex items-center justify-between gap-3">
        <div className="grid flex-1 grid-cols-2 gap-2 sm:grid-cols-4">
          <DetailStat label="Documents" value={formatCompact(documents.length)} />
          <DetailStat label="Chunks" value={formatCompact(collection.chunk_count)} />
          <DetailStat label="Estimated size" value={formatBytes(collection.estimated_bytes)} />
          <DetailStat label="Chunk tokens" value={`${collection.chunk_tokens} / ${collection.chunk_overlap_tokens} overlap`} />
        </div>
        {!editing && (
          <Button type="button" size="sm" variant="outline" onClick={openEdit} className="shrink-0 gap-1.5">
            <Pencil className="h-3.5 w-3.5" />
            Edit
          </Button>
        )}
      </div>

      {editing && (
        <div className="space-y-3 rounded-md border border-border/70 bg-background/30 p-3">
          <div className="grid gap-3 sm:grid-cols-2">
            <div className="space-y-1.5">
              <Label htmlFor="kb-edit-name">Name</Label>
              <Input id="kb-edit-name" value={editName} onChange={(e) => setEditName(e.target.value)} disabled={editSaving} />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="kb-edit-description">Description</Label>
              <Input
                id="kb-edit-description"
                value={editDescription}
                onChange={(e) => setEditDescription(e.target.value)}
                disabled={editSaving}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="kb-edit-embedding-model">Embedding model</Label>
              <Input
                id="kb-edit-embedding-model"
                value={editEmbeddingModel}
                onChange={(e) => setEditEmbeddingModel(e.target.value)}
                disabled={editSaving}
              />
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div className="space-y-1.5">
                <Label htmlFor="kb-edit-chunk-tokens">Chunk tokens</Label>
                <Input
                  id="kb-edit-chunk-tokens"
                  type="number"
                  min="1"
                  value={editChunkTokens}
                  onChange={(e) => setEditChunkTokens(e.target.value)}
                  disabled={editSaving}
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="kb-edit-chunk-overlap-tokens">Overlap tokens</Label>
                <Input
                  id="kb-edit-chunk-overlap-tokens"
                  type="number"
                  min="0"
                  value={editChunkOverlapTokens}
                  onChange={(e) => setEditChunkOverlapTokens(e.target.value)}
                  disabled={editSaving}
                />
              </div>
            </div>
          </div>
          <p className="text-[11px] leading-relaxed text-muted-foreground">
            Changing the embedding model or either chunk size does not alter chunks that already
            exist — the new values only take effect the next time this collection is reindexed.
          </p>
          {editError && (
            <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive">
              {editError}
            </p>
          )}
          <div className="flex items-center gap-2">
            <Button type="button" size="sm" onClick={() => void handleSaveEdit()} disabled={editSaving}>
              {editSaving ? "Saving…" : "Save"}
            </Button>
            <Button type="button" size="sm" variant="ghost" onClick={() => setEditing(false)} disabled={editSaving}>
              Cancel
            </Button>
          </div>
        </div>
      )}

      {staleEmbedder && (
        <div className="flex items-start justify-between gap-3 rounded-md border border-amber-500/30 bg-amber-500/10 px-3 py-2.5 text-xs text-amber-200">
          <div className="flex items-start gap-2">
            <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
            <p className="leading-relaxed">
              This collection needs re-indexing: its documents were embedded with{" "}
              <span className="font-medium">{collection.indexed_embedding_model || "an earlier model"}</span>,
              but the collection is now configured to use{" "}
              <span className="font-medium">{collection.embedding_model}</span>. Retrieval is
              currently running on the old vectors.
            </p>
          </div>
          <Button size="sm" variant="outline" onClick={handleReindexCollection} disabled={reindexPending}>
            <RefreshCw className={cn("h-3.5 w-3.5", reindexPending && "animate-spin")} />
            {reindexPending ? "Reindexing…" : "Reindex now"}
          </Button>
        </div>
      )}

      {failedDocs.length > 0 && (
        <div className="rounded-md border border-destructive/35 bg-destructive/10 px-3 py-2.5 text-xs text-destructive">
          <div className="flex items-center gap-2 font-medium">
            <AlertTriangle className="h-3.5 w-3.5" />
            {failedDocs.length} document{failedDocs.length === 1 ? "" : "s"} failed to index
          </div>
          <p className="mt-1 leading-relaxed text-destructive/85">
            Reindex a document after fixing the underlying issue (an unsupported file type, or a
            transient embedding-model failure).
          </p>
        </div>
      )}

      {message && (
        <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
          <p>{message.text}</p>
          {message.showReindex && !staleEmbedder && (
            <Button size="sm" variant="outline" onClick={handleReindexCollection} disabled={reindexPending}>
              <RefreshCw className={cn("h-3.5 w-3.5", reindexPending && "animate-spin")} />
              {reindexPending ? "Reindexing…" : "Reindex now"}
            </Button>
          )}
        </div>
      )}

      <DocumentUpload
        collectionId={collection.id}
        maxUploadBytes={maxUploadBytes}
        onUploaded={(doc) => {
          setDocuments((prev) => [doc, ...prev]);
          onChanged();
        }}
      />

      <RetrievalPreview
        collection={collection}
        documents={documents}
        minScore={minScore}
        maxContextTokens={maxContextTokens}
      />

      {documents.length === 0 ? (
        <EmptyState className="h-32 flex-col gap-1">
          <span>No documents in this collection yet.</span>
          <span className="text-xs">Upload a file above to start indexing.</span>
        </EmptyState>
      ) : (
        <div className="overflow-hidden rounded-lg border border-border/70 bg-card/45">
          <div className="hidden grid-cols-[minmax(0,1fr)_7rem_6rem_6rem_2.5rem] border-b border-border/70 bg-background/35 px-4 py-2.5 text-xs font-medium text-muted-foreground md:grid">
            <div>Document</div>
            <div>Size</div>
            <div>Chunks</div>
            <div>Status</div>
            <div />
          </div>
          <div className="divide-y divide-border/60">
            {sortedDocuments.map((doc) => {
              const busy = busyIds.has(doc.id);
              return (
                <div
                  key={doc.id}
                  className="grid gap-2 px-4 py-3 md:grid-cols-[minmax(0,1fr)_7rem_6rem_6rem_auto] md:items-center"
                >
                  <div className="min-w-0">
                    <p className="truncate text-sm font-medium" title={doc.title}>
                      {doc.title}
                    </p>
                    <p className="truncate text-[11px] text-muted-foreground">{doc.content_type || doc.filename}</p>
                    {doc.status === "failed" && doc.error && (
                      <p className="mt-1 text-xs leading-relaxed text-destructive">{doc.error}</p>
                    )}
                  </div>
                  <div className="text-sm text-muted-foreground md:text-left">{formatBytes(doc.byte_size)}</div>
                  <div className="text-sm text-muted-foreground md:text-left">{formatCompact(doc.chunk_count)}</div>
                  <div>{statusBadge(doc)}</div>
                  <div className="flex items-center justify-end gap-1">
                    <button
                      type="button"
                      title="Reindex document"
                      onClick={() => void handleReindexDocument(doc)}
                      disabled={busy}
                      className="flex h-7 w-7 items-center justify-center rounded-md border border-border/70 bg-background/45 text-muted-foreground transition-colors hover:border-border hover:text-foreground disabled:opacity-50"
                    >
                      <RefreshCw className={cn("h-3.5 w-3.5", busy && "animate-spin")} />
                    </button>
                    <button
                      type="button"
                      title="Delete document"
                      onClick={() => void handleDeleteDocument(doc)}
                      disabled={busy}
                      className="flex h-7 w-7 items-center justify-center rounded-md border border-destructive/40 bg-background/45 text-destructive/70 transition-colors hover:border-destructive hover:text-destructive disabled:opacity-50"
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}

      <div className="flex justify-end">
        <Button type="button" variant="ghost" onClick={onClose}>
          Close
        </Button>
      </div>
    </div>
  );
}
