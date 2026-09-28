import { KnowledgeList } from "@/components/knowledge/knowledge-list";
import { requireAdmin } from "@/lib/auth/roles";
import { loadKnowledge } from "@/lib/knowledge-data";

export const dynamic = "force-dynamic";

export default async function KnowledgePage({ searchParams }: { searchParams: Promise<{ tab?: string }> }) {
  await requireAdmin();
  const { tab } = await searchParams;
  return <KnowledgeList data={await loadKnowledge()} tab={tab === "retrieval" ? "retrieval" : "collections"} />;
}
