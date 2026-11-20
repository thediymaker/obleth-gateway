import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";

// The answering gateway replica's capacity discovery state, polled by the
// model page's capacity tab.
export const GET = liveRoute(() => obleth.capacityDiscovery());
