import type { NextRequest } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

// Streams the model manifest to the browser as a JSON download. Admin-only:
// the proxy middleware only checks session presence, so this route
// independently enforces the admin role. Unlike the config backup this file
// carries no secrets — upstream keys are reported as a presence flag only — so
// it is safe to keep in a repository. `?names=a,b` exports just those models.
export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const full = await obleth.exportModels();
    const names = (req.nextUrl.searchParams.get("names") ?? "").split(",").map((n) => n.trim()).filter(Boolean);
    const wanted = new Set(names);
    const manifest = names.length ? { ...full, models: full.models.filter((m) => wanted.has(m.model_name)) } : full;
    const stamp = new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-");
    const filename = names.length === 1 ? `obleth-model-${names[0]}-${stamp}.json` : `obleth-models-${stamp}.json`;

    return new Response(JSON.stringify(manifest, null, 2), {
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
