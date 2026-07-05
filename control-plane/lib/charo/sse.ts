/** Frame one Server-Sent Event. Mirrors the existing chat route's framing. */
export function sse(event: string, data: unknown): string {
  return `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
}
