import { NextResponse } from "next/server";
import { z } from "zod";
import { liveRoute, RouteError } from "@/lib/auth/guard";
import { gatewaySpeech, type GatewaySpeechBody } from "@/lib/charo/gateway";
import { apiErrorMessage } from "@/lib/charo/errors";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// The playground's text-to-speech relay. Calls the gateway's normal
// /v1/audio/speech data-plane endpoint as the reserved internal tenant, so
// admission, billing, and telemetry behave exactly as for a real client.
//
// It always asks for WAV sent as raw bytes (`stream_format: "audio"`): the
// page decodes WAV as it arrives and plays it before the response is done.
// A backend that streams sends a header with unknown sizes and then PCM as it
// is made; one that does not sends the whole file at the end. Either way the
// bytes are passed through untouched and unbuffered, and the client's abort
// (Stop, or leaving the page) cancels the upstream request.
const bodySchema = z.object({
  model: z.string().min(1).max(200),
  input: z.string().min(1).max(20_000),
  voice: z.string().max(200).optional(),
  instructions: z.string().max(2000).optional(),
});

export const POST = liveRoute(async (req) => {
  const parsed = bodySchema.safeParse(await req.json().catch(() => null));
  if (!parsed.success || !parsed.data.input.trim()) throw new RouteError(400, "Invalid speech request.");
  const { model, input, voice, instructions } = parsed.data;

  const body: GatewaySpeechBody = {
    model,
    input,
    response_format: "wav",
    stream_format: "audio",
    ...(voice?.trim() ? { voice: voice.trim() } : {}),
    ...(instructions?.trim() ? { instructions: instructions.trim() } : {}),
  };

  const upstream = await gatewaySpeech(body, req.signal);
  const requestId = upstream.headers.get("x-obleth-request-id");
  if (!upstream.ok) {
    const json = await upstream.json().catch(() => null);
    return NextResponse.json({ error: apiErrorMessage(json, `speech failed (HTTP ${upstream.status})`), requestId }, { status: upstream.status });
  }
  if (!upstream.body) return NextResponse.json({ error: "The model returned no audio.", requestId }, { status: 502 });
  const headers = new Headers({
    "content-type": upstream.headers.get("content-type") || "audio/wav",
    "cache-control": "no-store",
    // Stop any proxy in front of the dashboard from holding the audio back
    // until the response is complete.
    "x-accel-buffering": "no",
  });
  if (requestId) headers.set("x-obleth-request-id", requestId);
  return new Response(upstream.body, { status: 200, headers });
});
