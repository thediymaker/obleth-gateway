import { requireAdmin } from "@/lib/auth/roles";
import { FailuresView } from "@/components/failures/failures-view";
import { isFailureRange, rangeOf, type FailureRange } from "@/lib/failures-model";
import { obleth, type UsageFailureGroups } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function FailuresPage({ searchParams }: { searchParams: Promise<{ range?: string }> }) {
  await requireAdmin();
  const q = await searchParams;
  // The Overview's failures tile links here with its own range.
  const range: FailureRange = isFailureRange(q.range) ? q.range : "24h";
  const initial = await safe(obleth.usageFailures({ sinceMs: Date.now() - rangeOf(range).ms }).then((d) => d as UsageFailureGroups | null), null);
  return <FailuresView key={range} initialRange={range} initial={initial} />;
}
