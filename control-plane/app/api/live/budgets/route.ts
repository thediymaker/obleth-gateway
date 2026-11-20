import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";

// Every capped budget and how much of its current period is used.
export const GET = liveRoute(() => obleth.budgetUsage());
