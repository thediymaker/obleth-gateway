import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

// Streams the gateway's config backup to the browser as a JSON download.
// Admin-only: the proxy middleware only checks session presence, so this route
// independently enforces the admin role before exposing config secrets. The
// admin-API bearer token is attached by the server-side obleth client.
export async function GET() {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const backup = await obleth.exportBackup();
    const stamp = new Date()
      .toISOString()
      .slice(0, 19)
      .replace(/[:T]/g, "-");
    const filename = `obleth-backup-${stamp}.json`;

    return new Response(JSON.stringify(backup, null, 2), {
      status: 200,
      headers: {
        "Content-Type": "application/json",
        "Content-Disposition": `attachment; filename="${filename}"`,
      },
    });
  } catch (e) {
    return new Response(JSON.stringify({ error: String(e) }), {
      status: 502,
      headers: { "Content-Type": "application/json" },
    });
  }
}
