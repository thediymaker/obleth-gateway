import { liveRoute } from "@/lib/auth/guard";
import { loadTenantOverview } from "@/lib/tenant-overview-data";

// A tenant page's traffic, spend, models, keys and recent changes, in one read.
export const GET = liveRoute<{ id: string }>((_req, { params }) => loadTenantOverview(params.id));
