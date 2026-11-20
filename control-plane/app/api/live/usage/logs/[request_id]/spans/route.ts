import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";

export const GET = liveRoute<{ request_id: string }>((_req, { params }) => obleth.getRequestSpans(params.request_id));
