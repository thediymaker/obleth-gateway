import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";

// Streams the gateway's config backup to the browser as a JSON download,
// passing its body through as it arrives rather than parsing and re-writing
// it. Admin-only: the proxy middleware only checks session presence, so this
// route independently enforces the admin role before exposing config secrets.
// The admin-API bearer token is attached by the server-side obleth client.
export const GET = liveRoute(async (_req, { session }) => {
  const res = await obleth.exportBackup({ auditActor: session.email });
  const stamp = new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-");
  const headers = new Headers({
    "Content-Type": res.headers.get("content-type") ?? "application/json",
    "Content-Disposition": res.headers.get("content-disposition") ?? `attachment; filename="obleth-backup-${stamp}.json"`,
    "Cache-Control": "no-store",
  });
  return new Response(res.body, { status: 200, headers });
});
