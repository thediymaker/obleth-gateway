import { KEY_REJECTION_ROWS, obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";
import { numParam, strParam } from "@/lib/log-params";

// Where keys were used from, and where they were refused, in one window.
export const GET = liveRoute(async (req) => {
  const p = req.nextUrl.searchParams;
  const sinceMs = numParam(p, "since_ms") || undefined;
  const tenantId = strParam(p, "tenant_id");
  const keyId = strParam(p, "key_id");
  const [sources, rejections] = await Promise.all([
    obleth.keySources({ sinceMs, tenantId, keyId }),
    obleth.keyRejections({ sinceMs, tenantId, keyId, limit: KEY_REJECTION_ROWS }),
  ]);
  return { sinceMs: sinceMs ?? null, sources, rejections };
});
