// Minimal MCP (streamable-HTTP) probe shared by Charo's test_mcp tool and the
// dashboard probe route. Speaks just enough JSON-RPC to verify a registered
// server end-to-end THROUGH the gateway (/mcp/{name}): initialize →
// notifications/initialized → tools/list. Dependency-free on purpose — the
// gateway stays transport-transparent, so the control plane is the only place
// that understands MCP framing.

import { getControlPlaneKey } from "@/lib/charo/gateway";
import type { McpServerTestRow, McpToolInfo } from "./types";

const PROXY_BASE = process.env.OBLETH_PROXY_BASE_URL ?? "http://localhost:8080";
const STEP_TIMEOUT_MS = 10_000;
const LEGACY_GET_TIMEOUT_MS = 3_000;
const PROTOCOL_VERSION = "2025-03-26";

interface RpcReply {
  status: number;
  sessionId: string | null;
  /** Parsed JSON-RPC envelope, or null for an empty/unparseable body. */
  body: { result?: unknown; error?: unknown } | null;
}

function joinSignals(caller: AbortSignal | undefined, ms: number): AbortSignal {
  const timeout = AbortSignal.timeout(ms);
  return caller ? AbortSignal.any([caller, timeout]) : timeout;
}

/** Error text from either shape: gateway `{error: "…"}` or JSON-RPC `{error: {message}}`. */
function errText(body: RpcReply["body"]): string | null {
  const e = body?.error;
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && typeof (e as { message?: unknown }).message === "string") {
    return (e as { message: string }).message;
  }
  return null;
}

async function rpcPost(
  url: string,
  key: string,
  sessionId: string | null,
  payload: Record<string, unknown>,
  signal: AbortSignal | undefined,
): Promise<RpcReply> {
  const res = await fetch(url, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${key}`,
      "Content-Type": "application/json",
      Accept: "application/json, text/event-stream",
      ...(sessionId ? { "Mcp-Session-Id": sessionId } : {}),
    },
    body: JSON.stringify(payload),
    signal: joinSignals(signal, STEP_TIMEOUT_MS),
  });
  const contentType = res.headers.get("content-type") ?? "";
  const newSession = res.headers.get("mcp-session-id");
  const text = await res.text().catch(() => "");

  let body: RpcReply["body"] = null;
  if (contentType.includes("text/event-stream")) {
    // Streamable-HTTP servers may answer a POST as a short SSE stream; the
    // reply is the `data:` line whose JSON-RPC id matches our request.
    const wantId = payload.id;
    for (const line of text.split("\n")) {
      const t = line.trim();
      if (!t.startsWith("data:")) continue;
      try {
        const parsed = JSON.parse(t.slice(5).trim()) as { id?: unknown };
        if (wantId === undefined || parsed.id === wantId) {
          body = parsed as RpcReply["body"];
          break;
        }
      } catch {
        /* keep scanning */
      }
    }
  } else if (text) {
    try {
      body = JSON.parse(text) as RpcReply["body"];
    } catch {
      body = null;
    }
  }
  return { status: res.status, sessionId: newSession ?? sessionId, body };
}

/** Legacy HTTP+SSE transport: POST is rejected but GET opens an event stream. */
async function looksLikeLegacySse(
  url: string,
  key: string,
  signal: AbortSignal | undefined,
): Promise<boolean> {
  try {
    const res = await fetch(url, {
      method: "GET",
      headers: { Authorization: `Bearer ${key}`, Accept: "text/event-stream" },
      signal: joinSignals(signal, LEGACY_GET_TIMEOUT_MS),
    });
    const ct = res.headers.get("content-type") ?? "";
    void res.body?.cancel().catch(() => {});
    return res.ok && ct.includes("text/event-stream");
  } catch {
    return false;
  }
}

/** Probe one registered MCP server through the gateway. Never throws for
 *  server-side problems — every outcome lands in the returned row. */
export async function probeServer(
  name: string,
  opts: { signal?: AbortSignal } = {},
): Promise<McpServerTestRow> {
  const row: McpServerTestRow = {
    server: name,
    status: "fail",
    message: "",
    serverInfo: null,
    protocolVersion: null,
    tools: null,
    latencyMs: null,
  };
  const url = `${PROXY_BASE}/mcp/${encodeURIComponent(name)}`;

  let key: string;
  try {
    key = await getControlPlaneKey();
  } catch (e) {
    row.message = `control-plane key unavailable: ${e instanceof Error ? e.message : String(e)}`;
    return row;
  }

  // ---- initialize ----
  const t0 = Date.now();
  let init: RpcReply;
  try {
    init = await rpcPost(
      url,
      key,
      null,
      {
        jsonrpc: "2.0",
        id: 1,
        method: "initialize",
        params: {
          protocolVersion: PROTOCOL_VERSION,
          capabilities: {},
          clientInfo: { name: "obleth-mcp-probe", version: "1" },
        },
      },
      opts.signal,
    );
  } catch (e) {
    row.message =
      e instanceof Error && e.name === "TimeoutError"
        ? `initialize timed out after ${STEP_TIMEOUT_MS / 1000}s`
        : `request failed: ${e instanceof Error ? e.message : String(e)}`;
    return row;
  }
  const initMs = Date.now() - t0;

  if (init.status === 404 || init.status === 405) {
    if (await looksLikeLegacySse(url, key, opts.signal)) {
      row.status = "warn";
      row.message = "legacy SSE transport — reachable, but the handshake needs streamable HTTP";
      return row;
    }
  }
  if (init.status < 200 || init.status >= 300) {
    const detail = errText(init.body);
    row.message = `HTTP ${init.status}${detail ? `: ${detail}` : ""}`;
    return row;
  }
  if (init.body?.error) {
    row.message = `initialize failed: ${errText(init.body) ?? "JSON-RPC error"}`;
    return row;
  }
  const initResult = (init.body?.result ?? null) as {
    protocolVersion?: unknown;
    serverInfo?: { name?: unknown; version?: unknown };
  } | null;
  if (!initResult) {
    row.message = "initialize returned no result (not an MCP server?)";
    return row;
  }
  row.protocolVersion =
    typeof initResult.protocolVersion === "string" ? initResult.protocolVersion : null;
  row.serverInfo = initResult.serverInfo
    ? {
        name: String(initResult.serverInfo.name ?? "unknown"),
        version: String(initResult.serverInfo.version ?? "?"),
      }
    : null;

  // ---- notifications/initialized (some servers require it before requests) ----
  try {
    await rpcPost(
      url,
      key,
      init.sessionId,
      { jsonrpc: "2.0", method: "notifications/initialized" },
      opts.signal,
    );
  } catch {
    /* non-fatal — plenty of servers accept requests without it */
  }

  // ---- tools/list ----
  const t1 = Date.now();
  let list: RpcReply | null = null;
  try {
    list = await rpcPost(
      url,
      key,
      init.sessionId,
      { jsonrpc: "2.0", id: 2, method: "tools/list" },
      opts.signal,
    );
  } catch {
    list = null;
  }
  row.latencyMs = { initialize: initMs, toolsList: list ? Date.now() - t1 : null };

  const listResult = (list?.body?.result ?? null) as { tools?: unknown } | null;
  const rawTools = Array.isArray(listResult?.tools) ? (listResult.tools as unknown[]) : null;
  if (!list || list.status < 200 || list.status >= 300 || list.body?.error || !rawTools) {
    const detail = list ? errText(list.body) : null;
    row.status = "warn";
    row.message = `initialize OK, but tools/list failed${detail ? `: ${detail}` : ""}`;
    return row;
  }
  row.tools = rawTools.map((t): McpToolInfo => {
    const o = (t ?? {}) as { name?: unknown; description?: unknown };
    return {
      name: String(o.name ?? "?"),
      description: typeof o.description === "string" ? o.description : null,
    };
  });
  row.status = "pass";
  row.message = `handshake OK — ${row.tools.length} tool${row.tools.length === 1 ? "" : "s"}`;
  return row;
}
