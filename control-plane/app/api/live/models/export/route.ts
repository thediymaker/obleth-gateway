import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";

// Streams the model manifest to the browser as a JSON download. Admin-only:
// the proxy middleware only checks session presence, so this route
// independently enforces the admin role. Unlike the config backup this file
// carries no secrets — upstream keys are reported as a presence flag only — so
// it is safe to keep in a repository. `?names=a,b` exports just those models.
export const GET = liveRoute(async (req) => {
  const names = (req.nextUrl.searchParams.get("names") ?? "").split(",").map((n) => n.trim()).filter(Boolean);
  const manifest = await obleth.exportModels(names);
  const stamp = new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-");
  const filename = names.length === 1 ? `obleth-model-${names[0]}-${stamp}.json` : `obleth-models-${stamp}.json`;
  return new Response(JSON.stringify(manifest, null, 2), {
    status: 200,
    headers: {
      "Content-Type": "application/json",
      "Content-Disposition": `attachment; filename="${filename}"`,
    },
  });
});
