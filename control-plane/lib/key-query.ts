// The Keys list's gateway query, shared by the server client and the page.

/** What a page of the Keys list matches and its order (`GET /keys`). */
export interface KeyListQuery {
  tenantId?: string;
  /** Only these keys (up to 200). */
  ids?: string[];
  /** Name, prefix, description, identity or tenant name contains it. */
  q?: string;
  disabled?: boolean;
  kind?: "secret" | "identity";
  budget?: "capped" | "none" | "near";
  /** No use in the window. */
  unused?: boolean;
  sort?: "created" | "name" | "last_used" | "requests" | "tokens" | "cost" | "budget";
  dir?: "asc" | "desc";
  /** The use window; send the same one with every page. */
  sinceMs?: number;
  untilMs?: number;
}

/** `GET /keys` query parameters for a {@link KeyListQuery}. */
export function keyQueryParams(k: KeyListQuery): Record<string, string | number | undefined> {
  return {
    tenant_id: k.tenantId || undefined,
    ids: k.ids?.length ? k.ids.join(",") : undefined,
    q: k.q?.trim() || undefined,
    disabled: k.disabled === undefined ? undefined : String(k.disabled),
    kind: k.kind,
    budget: k.budget,
    unused: k.unused ? "true" : undefined,
    sort: k.sort,
    dir: k.dir,
    since_ms: k.sinceMs,
    until_ms: k.untilMs,
  };
}
