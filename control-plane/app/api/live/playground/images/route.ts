import { z } from "zod";
import { liveRoute, RouteError } from "@/lib/auth/guard";
import { gatewayImages, type GatewayImagesBody } from "@/lib/charo/gateway";
import { apiErrorMessage } from "@/lib/charo/errors";
import { imageUrls } from "@/lib/charo/images";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// The playground's image-generation relay. Calls the gateway's normal
// /v1/images/generations data-plane endpoint as the reserved internal tenant,
// so admission, billing, and telemetry behave exactly as for a real client.
//
// `negative_prompt`, `steps`, and `seed` are NOT OpenAI-standard fields. They
// are forwarded as extra body fields because ComfyUI bridges and sd-webui
// OpenAI shims generally accept them; a backend that ignores them still
// generates. The panel labels them as backend-dependent for the same reason.
const bodySchema = z.object({
  model: z.string().min(1).max(200),
  prompt: z.string().min(1).max(4000),
  negative_prompt: z.string().max(4000).optional(),
  size: z.string().max(20).optional(),
  n: z.number().int().min(1).max(4).optional(),
  steps: z.number().int().min(1).max(150).optional(),
  seed: z.number().int().min(0).max(4_294_967_295).optional(),
});

export const POST = liveRoute(async (req) => {
  const parsed = bodySchema.safeParse(await req.json().catch(() => null));
  if (!parsed.success) throw new RouteError(400, "Invalid generation request.");
  const { model, prompt, negative_prompt, size, n, steps, seed } = parsed.data;

  const body: GatewayImagesBody = {
    model,
    prompt,
    n: n ?? 1,
    response_format: "b64_json",
    ...(size ? { size } : {}),
    ...(negative_prompt ? { negative_prompt } : {}),
    ...(steps !== undefined ? { steps } : {}),
    ...(seed !== undefined ? { seed } : {}),
  };

  const started = Date.now();
  const upstream = await gatewayImages(body, req.signal);
  const latencyMs = Date.now() - started;
  const requestId = upstream.headers.get("x-obleth-request-id");
  const json = await upstream.json().catch(() => null);
  if (!upstream.ok) throw new RouteError(upstream.status, apiErrorMessage(json, "generation failed"));
  const images = imageUrls(json);
  if (images.length === 0) throw new RouteError(502, "The model returned no image.");
  return { images, latencyMs, requestId };
});
