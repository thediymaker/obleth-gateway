import { fetchOverviewSummary } from "@/lib/overview-summary";
import { liveRoute } from "@/lib/auth/guard";

export const GET = liveRoute(() => fetchOverviewSummary());
