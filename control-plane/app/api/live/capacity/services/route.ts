import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";
import { strParam } from "@/lib/log-params";

// The Services the kubernetes capacity source can see (cached briefly by the
// gateway), and the model's default Service when `?model=` names one. Read by
// the Service picker on the model page's capacity tab.
export const GET = liveRoute((req) => obleth.capacityServices(strParam(req.nextUrl.searchParams, "model")));
