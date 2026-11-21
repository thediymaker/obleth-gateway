import { liveRoute } from "@/lib/auth/guard";
import { parseKeyListParams } from "@/lib/access-model";
import { loadKeyPage } from "@/lib/key-page-data";
import { strParam } from "@/lib/log-params";

// One page of the Keys list for its "Load more" and its searches, filters
// and sorts: the gateway matches and orders every key; `cursor` continues.
export const GET = liveRoute(async (req) => {
  const p = req.nextUrl.searchParams;
  return loadKeyPage(parseKeyListParams(p), strParam(p, "cursor"));
});
