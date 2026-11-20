import { afterEach, describe, expect, it, vi } from "vitest";

afterEach(() => vi.resetModules());

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
  vi.doMock("@/lib/auth/roles", () => ({
    requireAdmin: async () => ({ id: "admin-1", email: "admin@example.com", role: "admin", status: "active", tenantId: null }),
  }));
  vi.doMock("next/cache", () => ({ revalidatePath }));
  vi.doMock("@/lib/obleth", () => ({ obleth, OblethApiError }));
  return { revalidatePath };
}

const refused = () => vi.fn().mockRejectedValue(new OblethApiError(422, "weight must be at least 1", "/x"));

describe("actions answer a gateway refusal instead of throwing", () => {
  it("setWeightAction", async () => {
    const { revalidatePath } = setup({ setWeight: refused() });
    const { setWeightAction } = await import("./actions");
    expect(await setWeightAction("t1", 0)).toEqual({ ok: false, error: "weight must be at least 1" });
    expect(revalidatePath).not.toHaveBeenCalled();
  });

  it("autotuneModelAction", async () => {
    setup({ autotuneModel: refused() });
    const { autotuneModelAction } = await import("./actions");
    expect(await autotuneModelAction("m1")).toEqual({ ok: false, error: "weight must be at least 1" });
  });

  it("autotuneModelAction returns its report", async () => {
    const report = { recommended_max_in_flight: 12 };
    setup({ autotuneModel: vi.fn().mockResolvedValue(report) });
    const { autotuneModelAction } = await import("./actions");
    expect(await autotuneModelAction("m1")).toEqual({ ok: true, report });
  });

  it("applyAutotuneCapacityAction", async () => {
    setup({ applyAutotuneCapacity: refused() });
    const { applyAutotuneCapacityAction } = await import("./actions");
    expect(await applyAutotuneCapacityAction("m1", 12)).toEqual({ ok: false, error: "weight must be at least 1" });
  });
});

describe("bulk key delete", () => {
  it("names each key that failed and why, and still refreshes the pages", async () => {
    const keys = ["a", "b", "c"].map((id) => ({ id, name: `key-${id}`, key_prefix: `obl_${id}` }));
    const deleteKey = vi.fn(async (id: string) => {
      if (id === "b") throw new OblethApiError(409, "key is in use by a batch", "/keys/b");
    });
    const keysByIds = vi.fn().mockResolvedValue(keys);
    const { revalidatePath } = setup({ keysByIds, deleteKey });
    const { deleteKeysAction } = await import("./actions");
    const result = await deleteKeysAction(["a", "b", "c"]);
    expect(keysByIds).toHaveBeenCalledWith(["a", "b", "c"]);
    expect(result).toEqual({ done: 2, failed: [{ name: "key-b", error: "key is in use by a batch" }] });
    expect(deleteKey).toHaveBeenCalledTimes(3);
    expect(revalidatePath).toHaveBeenCalledWith("/keys");
  });

  it("refreshes the pages even when the key list cannot be read", async () => {
    const { revalidatePath } = setup({ keysByIds: vi.fn().mockRejectedValue(new Error("gateway down")), deleteKey: vi.fn() });
    const { deleteKeysAction } = await import("./actions");
    expect(await deleteKeysAction(["a"])).toEqual({ done: 0, failed: [{ name: "all", error: "gateway down" }] });
    expect(revalidatePath).toHaveBeenCalledWith("/keys");
  });
});
