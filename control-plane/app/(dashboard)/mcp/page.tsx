import { McpList } from "@/components/mcp/mcp-list";
import { requireAdmin } from "@/lib/auth/roles";
import { obleth, type DailyStatsView, type McpServer, type ModelRoute } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function McpPage() {
  await requireAdmin();
  const [servers, models, stats] = await Promise.all([
    safe(obleth.listMcpServers(), [] as McpServer[]),
    safe(obleth.listModels(), [] as ModelRoute[]),
    safe<DailyStatsView | null>(obleth.dailyStats("mcp", 7), null),
  ]);
  return <McpList servers={servers} models={models} initialStats={stats} />;
}
