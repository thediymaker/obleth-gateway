import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";

// Streams the gateway's config backup to the browser as a JSON download.
// Admin-only: the proxy middleware only checks session presence, so this route
// independently enforces the admin role before exposing config secrets. The
// admin-API bearer token is attached by the server-side obleth client.
export const GET = liveRoute(async () => {
  const backup = await obleth.exportBackup();
  const stamp = new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-");
  return new Response(JSON.stringify(backup, null, 2), {
    status: 200,
    headers: {
      "Content-Type": "application/json",
      "Content-Disposition": `attachment; filename="obleth-backup-${stamp}.json"`,
    },
  });
});
