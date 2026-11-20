import { liveRoute, RouteError } from "@/lib/auth/guard";
import { gatewayMeshCreate } from "@/lib/charo/gateway";
import { apiErrorMessage } from "@/lib/charo/errors";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// The playground's 3D mesh relay. Starts a job on the gateway's normal
// /v1/meshes data-plane endpoint as the reserved internal tenant, so
// admission, billing, and telemetry behave exactly as for a real client. The
// page then polls ./[id] and downloads ./[id]/content.
//
// Only the fields mesh backends commonly take are forwarded; anything else in
// the form is dropped rather than passed through.
const MAX_IMAGE_BYTES = 20 * 1024 * 1024;
const IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/webp"]);
const SETTINGS: Record<string, { min: number; max: number }> = {
  resolution: { min: 256, max: 2048 },
  faces: { min: 1_000, max: 2_000_000 },
  texture_size: { min: 256, max: 8192 },
  seed: { min: 0, max: 4_294_967_295 },
};

export const POST = liveRoute(async (req) => {
  const form = await req.formData().catch(() => null);
  const model = form?.get("model");
  const image = form?.get("image");
  if (!form || typeof model !== "string" || !model.trim() || model.length > 200 || !(image instanceof File)) {
    throw new RouteError(400, "Choose a model and an image.");
  }
  if (!IMAGE_TYPES.has(image.type) || image.size > MAX_IMAGE_BYTES) {
    throw new RouteError(400, "The image must be a PNG, JPEG or WebP of at most 20 MB.");
  }

  const upstream = new FormData();
  upstream.set("model", model);
  upstream.set("image", image, image.name || "image.png");
  for (const [name, range] of Object.entries(SETTINGS)) {
    const raw = form.get(name);
    if (typeof raw !== "string" || !raw.trim()) continue;
    const n = Number(raw);
    if (!Number.isInteger(n) || n < range.min || n > range.max) {
      throw new RouteError(400, `Invalid ${name.replace("_", " ")}.`);
    }
    upstream.set(name, String(n));
  }

  const res = await gatewayMeshCreate(upstream);
  const json = await res.json().catch(() => null);
  if (!res.ok) throw new RouteError(res.status, apiErrorMessage(json, "the mesh job could not be started"));
  return { job: json, requestId: res.headers.get("x-obleth-request-id") };
});
