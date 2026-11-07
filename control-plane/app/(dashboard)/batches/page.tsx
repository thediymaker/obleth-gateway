import { requireAdmin } from "@/lib/auth/roles";
import { BatchesView } from "@/components/batches/batches-view";
import { BATCH_FILTERS, isBatchFilter, type BatchFilter } from "@/lib/batches-model";
import { obleth, type AdminBatchList } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function BatchesPage({ searchParams }: { searchParams: Promise<{ show?: string }> }) {
  await requireAdmin();
  const q = await searchParams;
  const filter: BatchFilter = isBatchFilter(q.show) ? q.show : "active";
  const initial = await safe(
    obleth.listBatches({ status: BATCH_FILTERS[filter].status, limit: 100 }).then((d) => d as AdminBatchList | null),
    null,
  );
  return <BatchesView key={filter} initialFilter={filter} initial={initial} />;
}
