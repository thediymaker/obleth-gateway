import { notFound, redirect } from "next/navigation";
import { CollectionPage } from "@/components/knowledge/collection-page";
import { requireAdmin } from "@/lib/auth/roles";
import { loadKnowledge } from "@/lib/knowledge-data";
import { collectionHref } from "@/lib/knowledge-format";
import { obleth, type AuditEntry } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function KnowledgeCollectionPage({ params }: { params: Promise<{ name: string }> }) {
  await requireAdmin();
  const name = decodeURIComponent((await params).name);
  const data = await loadKnowledge();
  const collection = data.collections.find((c) => c.name === name) ?? data.collections.find((c) => c.id === name);
  if (!collection) notFound();
  if (collection.name !== name) redirect(collectionHref(collection.name));
  const docIds = new Set((data.documents[collection.id] ?? []).map((d) => d.id));
  // The collection's own changes by id, and its documents' from the type's
  // newest 200: both answered from the audit log's entity index.
  const [collectionChanges, documentChanges] = await Promise.all([
    safe(obleth.auditQuery({ entityType: "knowledge_collection", entityId: collection.id, limit: 12 }), [] as AuditEntry[]),
    safe(obleth.auditQuery({ entityType: "knowledge_document", limit: 200 }), [] as AuditEntry[]),
  ]);
  const changes = [...collectionChanges, ...documentChanges.filter((e) => docIds.has(e.entity_id))].sort((a, b) => b.id - a.id).slice(0, 12);
  return (
    <CollectionPage
      key={collection.id}
      collection={collection}
      initialDocuments={data.documents[collection.id] ?? []}
      settings={data.settings}
      models={data.models}
      usedBy={data.usedBy[collection.id] ?? []}
      stats={data.stats}
      changes={changes}
    />
  );
}
