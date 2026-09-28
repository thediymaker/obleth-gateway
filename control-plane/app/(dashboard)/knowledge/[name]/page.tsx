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
  const changes = await safe(obleth.auditQuery({ limit: 200 }), [] as AuditEntry[]);
  return (
    <CollectionPage
      key={collection.id}
      collection={collection}
      initialDocuments={data.documents[collection.id] ?? []}
      settings={data.settings}
      models={data.models}
      usedBy={data.usedBy[collection.id] ?? []}
      stats={data.stats}
      changes={changes.filter((e) => (e.entity_type === "knowledge_collection" && e.entity_id === collection.id) || (e.entity_type === "knowledge_document" && docIds.has(e.entity_id))).slice(0, 12)}
    />
  );
}
