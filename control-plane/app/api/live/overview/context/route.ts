import { liveRoute } from "@/lib/auth/guard";
import { loadOverviewContext } from "@/lib/overview-data";

export const GET = liveRoute(() => loadOverviewContext());
