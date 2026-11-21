import { liveRoute } from "@/lib/auth/guard";
import { parseKeyListParams } from "@/lib/access-model";
import { obleth } from "@/lib/obleth";

// How many keys the Keys list's filters match, for "select every match" and
// its confirm.
export const GET = liveRoute(async (req) => obleth.countKeys(parseKeyListParams(req.nextUrl.searchParams)));
