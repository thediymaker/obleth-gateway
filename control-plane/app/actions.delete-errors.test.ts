import { afterEach, describe, expect, it, vi } from "vitest";

afterEach(() => vi.resetModules());

const EVICTION_MESSAGE =
  "deleted, but evicting it from the data-plane cache failed; reconcile with POST /api/v1/resync";

class OblethApiError extends Error {
  constructor(
    public readonly status: number,
    message: string,
    public readonly path: string,
  ) {
    super(message);
  }
}

function setup(obleth: Record<string, unknown>) {
  const revalidatePath = vi.fn();
  const updateTag = vi.fn();
  vi.doMock("@/lib/auth/roles", () => ({
    requireAdmin: async () => ({ id: "admin-1", email: "admin@example.com", role: "admin", status: "active", tenantId: null }),
  }));
  vi.doMock("next/cache", () => ({ revalidatePath, updateTag }));
  vi.doMock("@/lib/obleth", () => ({
    obleth,
    CACHE_TAGS: new Proxy({}, { get: (_t, prop) => String(prop) }),
    OblethApiError,
  }));
  return { revalidatePath, updateTag };
}

const cases = [
  { action: "deleteTenantAction", method: "deleteTenant", path: "/tenants", tag: "tenants" },
  { action: "deleteKeyAction", method: "deleteKey", path: "/keys", tag: "keys" },
  { action: "deleteModelAction", method: "deleteModel", path: "/models", tag: "models" },
  { action: "deleteMcpServerAction", method: "deleteMcpServer", path: "/mcp", tag: null },
] as const;

describe("delete actions surface a failed cache eviction", () => {
  for (const { action, method, path, tag } of cases) {
    it(`${action} returns the 502 message and still revalidates`, async () => {
      const del = vi.fn().mockRejectedValue(new OblethApiError(502, EVICTION_MESSAGE, "/x"));
      const { revalidatePath, updateTag } = setup({ [method]: del });
      const actions = await import("./actions");
      const result = await (actions[action] as (id: string) => Promise<unknown>)("id-1");

      expect(result).toEqual({ ok: false, error: EVICTION_MESSAGE });
      expect(del).toHaveBeenCalledWith("id-1", { auditActor: "admin@example.com" });
      expect(revalidatePath).toHaveBeenCalledWith(path);
      if (tag) expect(updateTag).toHaveBeenCalledWith(tag);
    });

    it(`${action} reports success`, async () => {
      const del = vi.fn().mockResolvedValue(undefined);
      const { revalidatePath } = setup({ [method]: del });
      const actions = await import("./actions");
      const result = await (actions[action] as (id: string) => Promise<unknown>)("id-1");

      expect(result).toEqual({ ok: true });
      expect(revalidatePath).toHaveBeenCalledWith(path);
    });
  }
});
