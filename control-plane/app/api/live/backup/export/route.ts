import { obleth } from "@/lib/obleth";

// Streams the gateway's config backup to the browser as a JSON download.
// Session auth is enforced by the proxy middleware like every /api/live route;
// the admin-API bearer token is attached by the server-side obleth client.
export async function GET() {
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
