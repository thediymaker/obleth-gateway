"use client";

import { useState, useTransition } from "react";
import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { AlertCircle, Plus } from "lucide-react";
import { setKnowledgeEnabledAction } from "@/app/actions";
import { Glyph } from "@/components/deployments/ui";
import { RetrievalSettings } from "@/components/knowledge/retrieval-settings";
import { Field, SelectField } from "@/components/models/fields";
import { Notice, Sheet, Tile } from "@/components/models/ui";
import { Pill } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { formatBytes } from "@/lib/format";
import type { KnowledgeData } from "@/lib/knowledge-data";
import { COLLECTION_STATUS_LABEL, collectionGlyph, collectionHref, collectionStatus, retrievalUse } from "@/lib/knowledge-format";
import { compact } from "@/lib/overview-model";
import { cn } from "@/lib/utils";

const COLS = "grid-cols-[minmax(0,1.5fr)_130px_70px_70px_minmax(0,1fr)_minmax(0,1fr)_120px_90px]";

export function StatusMark({ label, glyph }: { label: string; glyph: "on" | "half" | "off" | "attention" }) {
  if (glyph === "attention") return <span className="inline-flex h-[22px] items-center gap-1 whitespace-nowrap rounded-full bg-foreground px-2 text-[11.5px] font-semibold text-background"><AlertCircle className="h-3 w-3" aria-hidden />{label}</span>;
  return <Pill><Glyph glyph={glyph} className="h-[7px] w-[7px]" />{label}</Pill>;
}

function NewCollectionSheet({ open, onClose, embedders }: { open: boolean; onClose: () => void; embedders: string[] }) {
  const router = useRouter();
  const [model, setModel] = useState(embedders[0] ?? "");
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();
  return (
    <Sheet
      open={open}
      onClose={onClose}
      title="New collection"
      description="A set of documents models can quote from. Add documents on its page once it's made."
      width="w-[min(520px,100vw)]"
      footer={
        <div className="flex items-center justify-between gap-3">
          <span role="alert" className="text-[12.5px] font-medium">{error}</span>
          <div className="flex gap-2">
            <Button type="button" variant="outline" size="sm" onClick={onClose} disabled={pending}>Cancel</Button>
            <Button type="submit" form="new-collection-form" size="sm" disabled={pending}>{pending ? "Making…" : "Make collection"}</Button>
          </div>
        </div>
      }
    >
      <form
        id="new-collection-form"
        className="flex flex-col gap-4 px-6 pb-6"
        onSubmit={(e) => {
          e.preventDefault();
          const fd = new FormData(e.currentTarget);
          const name = String(fd.get("name") ?? "").trim();
          if (!name) return setError("Name it.");
          if (!model.trim()) return setError("Pick an embedding model.");
          setError(null);
          start(async () => {
            const res = await fetch("/api/live/knowledge/collections", {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ name, description: String(fd.get("description") ?? "").trim(), embedding_model: model.trim() }),
            });
            const body = await res.json().catch(() => null);
            if (!res.ok) return setError((body && body.error) || `That didn't work (HTTP ${res.status}).`);
            router.push(collectionHref(name));
          });
        }}
      >
        <Field label="Name" name="name" required placeholder="Supercomputing guides" />
        <Field label="Description" name="description" placeholder="Optional" />
        <div className="space-y-1.5">
          <span className="text-[12.5px] font-medium text-secondary-foreground">Embedding model</span>
          {embedders.length ? (
            <SelectField label="Embedding model" value={model} onChange={setModel} options={embedders.map((m) => ({ value: m, label: m }))} />
          ) : (
            <Field label="Embedding model" name="embedding_model_text" value={model} onChange={(e) => setModel(e.target.value)} placeholder="An embedding model's API name" />
          )}
          <p className="text-[11.5px] text-muted-foreground">Turns each chunk into a vector. Changing it later re-embeds everything.</p>
        </div>
      </form>
    </Sheet>
  );
}

export function KnowledgeList({ data, tab: initialTab }: { data: KnowledgeData; tab: "collections" | "retrieval" }) {
  const router = useRouter();
  const pathname = usePathname();
  const [tab, setTab] = useState(initialTab);
  const [adding, setAdding] = useState(false);
  const [notice, setNotice] = useState<{ text: string; strong?: boolean } | null>(null);
  const [pending, start] = useTransition();
  const { collections, documents, settings, usedBy, stats } = data;
  const docCount = collections.reduce((n, c) => n + (documents[c.id]?.length ?? 0), 0);
  const failed = collections.reduce((n, c) => n + (documents[c.id] ?? []).filter((d) => d.status === "failed").length, 0);
  const chunks = collections.reduce((n, c) => n + c.chunk_count, 0);
  const bytes = collections.reduce((n, c) => n + (documents[c.id] ?? []).reduce((m, d) => m + d.byte_size, 0), 0);
  const users = new Set(Object.values(usedBy).flat());
  const use = collections.map((c) => retrievalUse(stats?.items.find((i) => i.id === c.id)));
  const searches = use.reduce((n, u) => n + u.searches, 0);
  const hits = use.reduce((n, u) => n + u.hits, 0);
  const embedders = data.models.filter((m) => m.model_type === "embedding").map((m) => m.model_name).sort();
  const retrievalOn = !!settings?.enabled;

  const switchTab = (t: "collections" | "retrieval") => {
    setTab(t);
    router.replace(t === "retrieval" ? `${pathname}?tab=retrieval` : pathname, { scroll: false });
  };

  function turnOn() {
    start(async () => {
      const res = await setKnowledgeEnabledAction(true);
      setNotice(res.ok ? { text: "Retrieval is on. Models with the Knowledge boon and a collection now retrieve." } : { text: res.error, strong: true });
      router.refresh();
    });
  }

  const line = [
    `${collections.length} collection${collections.length === 1 ? "" : "s"}`,
    `${docCount} document${docCount === 1 ? "" : "s"}`,
    `${compact(chunks)} chunks`,
    users.size ? `used by ${users.size} model${users.size === 1 ? "" : "s"}` : "no model uses it yet",
  ].join(" · ");

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-[18px]">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          <h1 className="text-[26px] font-semibold tracking-tight">Knowledge</h1>
          <p className="text-[13px] text-secondary-foreground">{line}</p>
        </div>
        <Button type="button" size="sm" className="h-9" onClick={() => setAdding(true)}><Plus className="h-4 w-4" />New collection</Button>
      </div>

      {settings && !retrievalOn && (
        <section aria-label="Retrieval is off" className="flex flex-wrap items-center justify-between gap-4 rounded-xl border-[1.5px] border-foreground bg-card px-5 py-4">
          <div className="flex max-w-[780px] flex-col gap-1">
            <p className="text-[15px] font-semibold">Retrieval is off</p>
            <p className="text-[13px] leading-relaxed text-secondary-foreground">No model quotes these documents yet. Turn retrieval on here, then attach a collection to a chat model from the collection&apos;s page; attaching gives the model its Knowledge boon.</p>
          </div>
          <div className="flex gap-2">
            <Button type="button" variant="outline" size="sm" onClick={() => switchTab("retrieval")}>Retrieval settings</Button>
            <Button type="button" size="sm" disabled={pending} onClick={turnOn}>{pending ? "Turning on…" : "Turn retrieval on"}</Button>
          </div>
        </section>
      )}
      {notice && <Notice onDismiss={() => setNotice(null)} strong={notice.strong}>{notice.text}</Notice>}

      <div role="tablist" aria-label="Knowledge" className="flex gap-1 border-b border-border">
        {(["collections", "retrieval"] as const).map((t) => (
          <button key={t} type="button" role="tab" aria-selected={tab === t} onClick={() => switchTab(t)} className={cn("-mb-px border-b-2 px-3 py-2.5 text-[13.5px]", tab === t ? "border-foreground text-foreground" : "border-transparent text-muted-foreground hover:text-foreground")}>
            {t === "collections" ? `Collections · ${collections.length}` : "Retrieval settings"}
          </button>
        ))}
      </div>

      {tab === "retrieval" ? (
        settings ? <RetrievalSettings key={JSON.stringify(settings)} settings={settings} onSaved={() => router.refresh()} /> : <Notice strong>The gateway&apos;s knowledge settings couldn&apos;t be read.</Notice>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
            <Tile label="Collections" value={collections.length} detail={collections.length ? `${collections.filter((c) => collectionStatus(c, documents[c.id] ?? []) === "ready").length} indexed` : null} />
            <Tile label="Documents" value={docCount} detail={`${formatBytes(bytes)} of text · ${failed} failed`} emphasis={failed > 0} />
            <Tile label="Searches · 30 days" value={compact(searches)} detail={searches ? `${Math.round((hits / searches) * 100)}% found something` : retrievalOn ? "none yet" : "retrieval is off"} />
            <Tile label="Used by" value={`${users.size} model${users.size === 1 ? "" : "s"}`} detail={users.size ? [...users].slice(0, 3).join(", ") : "none has the Knowledge boon"} />
          </div>

          <section aria-label="Collections" className="overflow-hidden rounded-xl border border-border bg-card">
            <div className="overflow-x-auto">
              <div className="min-w-[1000px]">
                <div className={cn("grid items-center gap-3.5 px-[18px] py-2.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground", COLS)}>
                  <span>Collection</span><span>Status</span><span className="text-right">Docs</span><span className="text-right">Chunks</span><span>Embedded with</span><span>Used by</span><span className="text-right">Searches · 30d</span><span className="text-right">Size</span>
                </div>
                {collections.length === 0 && <p className="border-t border-border px-[18px] py-8 text-center text-[13px] text-muted-foreground">No collections yet. Make one, then upload the documents models should quote.</p>}
                {collections.map((c, i) => {
                  const docs = documents[c.id] ?? [];
                  const status = collectionStatus(c, docs);
                  const u = use[i];
                  return (
                    <Link key={c.id} href={collectionHref(c.name)} className={cn("grid min-h-[56px] items-center gap-3.5 border-t border-border px-[18px] py-2 text-[13px] hover:bg-muted/30", COLS)}>
                      <span className="flex min-w-0 flex-col"><span className="truncate font-medium">{c.name}</span><span className="truncate text-[11.5px] text-muted-foreground">{c.description || " "}</span></span>
                      <span><StatusMark label={docs.length ? COLLECTION_STATUS_LABEL[status] : "Empty"} glyph={collectionGlyph(status, docs.length)} /></span>
                      <span className="text-right font-mono text-[12px]">{docs.length}</span>
                      <span className="text-right font-mono text-[12px]">{compact(c.chunk_count)}</span>
                      <span className="truncate font-mono text-[12px]">{c.embedding_model}</span>
                      <span className="truncate text-[12.5px]">{usedBy[c.id]?.length ? usedBy[c.id].join(", ") : <span className="text-muted-foreground">no model yet</span>}</span>
                      <span className="text-right font-mono text-[12px]">{u.searches ? `${compact(u.searches)} · ${Math.round((u.hits / u.searches) * 100)}% found` : "—"}</span>
                      <span className="text-right font-mono text-[12px]">{formatBytes(c.estimated_bytes)}</span>
                    </Link>
                  );
                })}
              </div>
            </div>
            <div className="flex flex-wrap justify-between gap-2 border-t border-border px-[18px] py-3 text-[12.5px] text-muted-foreground">
              <span>Click a collection for its documents and a test search</span>
              <span>Filled dot indexed · half dot indexing · dashed empty · white pill needs you</span>
            </div>
          </section>
        </>
      )}

      <NewCollectionSheet key={adding ? "open" : "closed"} open={adding} onClose={() => setAdding(false)} embedders={embedders} />
    </div>
  );
}
