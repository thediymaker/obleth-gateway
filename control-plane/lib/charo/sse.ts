/** Frame one Server-Sent Event. */
export function sse(event: string, data: unknown): string {
  return `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
}

export type SseSend = (event: string, data: unknown) => void;

/**
 * A streamed SSE response that `run` writes events to. Events sent after the
 * client has gone are dropped. A throw from `run` becomes an `error` event,
 * and the stream closes when `run` settles.
 */
export function sseResponse(run: (send: SseSend) => Promise<void>): Response {
  const stream = new ReadableStream<Uint8Array>({
    async start(controller) {
      const enc = new TextEncoder();
      let gone = false;
      const send: SseSend = (event, data) => {
        if (gone) return;
        try {
          controller.enqueue(enc.encode(sse(event, data)));
        } catch {
          gone = true;
        }
      };
      try {
        await run(send);
      } catch (e) {
        send("error", { message: String(e) });
      } finally {
        if (!gone) {
          try {
            controller.close();
          } catch {
            /* already closed or cancelled */
          }
        }
      }
    },
  });
  return new Response(stream, {
    headers: { "Content-Type": "text/event-stream", "Cache-Control": "no-cache, no-transform", Connection: "keep-alive" },
  });
}
