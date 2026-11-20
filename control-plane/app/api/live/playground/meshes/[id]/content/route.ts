import { liveRoute, RouteError } from "@/lib/auth/guard";
import { gatewayMeshGet } from "@/lib/charo/gateway";
import { apiErrorMessage } from "@/lib/charo/errors";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// A finished mesh (binary glTF), streamed through untouched: tens of MB at
// high detail, so it is never buffered here.
export const GET = liveRoute<{ id: string }>(async (_req, { params }) => {
  const res = await gatewayMeshGet(params.id, true);
  if (!res.ok || !res.body) {
    const json = await res.json().catch(() => null);
    throw new RouteError(res.ok ? 502 : res.status, apiErrorMessage(json, "download failed"));
  }
  const headers = new Headers({ "Content-Type": res.headers.get("content-type") ?? "model/gltf-binary" });
  const length = res.headers.get("content-length");
  if (length) headers.set("Content-Length", length);
  return new Response(res.body, { status: 200, headers });
});
