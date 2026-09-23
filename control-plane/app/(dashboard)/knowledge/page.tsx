import { requireAdmin } from "@/lib/auth/roles";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";
import { CollectionList } from "@/components/knowledge/collection-list";

export const dynamic = "force-dynamic";

export default async function KnowledgePage() {
  await requireAdmin();
  const [collections, models, settings] = await Promise.all([
    safe(obleth.listCollections(), []),
    safe(obleth.listModels(), []),
    safe(obleth.getKnowledgeSettings(), null),
  ]);

  // Pre-load each collection's documents so the list can show an accurate
  // status badge (failed / indexing / needs re-index / ready) without the
  // operator having to open every collection to find out.
  const documentLists = await Promise.all(
    collections.map((c) => safe(obleth.listDocuments(c.id), [])),
  );
  const documentsByCollection = Object.fromEntries(
    collections.map((c, i) => [c.id, documentLists[i]]),
  );

  const embeddingModels = models
    .filter((m) => m.model_type === "embedding")
    .map((m) => m.model_name);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Knowledge</h1>
        <p className="text-sm text-muted-foreground">
          Collections of documents a model can retrieve from at request time.
          Each collection is chunked and embedded with a chosen embedding
          model; models are wired to collections from the model&apos;s own
          settings.
        </p>
      </div>
      <CollectionList
        collections={collections}
        documentsByCollection={documentsByCollection}
        embeddingModels={embeddingModels}
        maxUploadBytes={settings?.max_upload_bytes ?? null}
        minScore={settings?.min_score ?? null}
        maxContextTokens={settings?.max_context_tokens ?? null}
      />
    </div>
  );
}
