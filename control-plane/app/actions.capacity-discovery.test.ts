import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

// Same mocking approach as the other actions tests: actions.ts pulls in
// "next/cache", "@/lib/obleth" and "@/lib/auth/roles", none of which run
// under vitest.
async function discoveryAction(fd: FormData, reject?: Error) {
  vi.doMock("@/lib/auth/roles", () => ({
    requireAdmin: async () => ({
      id: "admin-1",
      email: "admin@example.com",
      role: "admin",
      status: "active",
      tenantId: null,
    }),
  }));
  vi.doMock("next/cache", () => ({ revalidatePath: vi.fn(), updateTag: vi.fn() }));
  class OblethApiError extends Error {}
  const setModelCapacityMode = reject
    ? vi.fn().mockRejectedValue(new OblethApiError(reject.message))
    : vi.fn().mockResolvedValue({});
  vi.doMock("@/lib/obleth", () => ({
    obleth: { setModelCapacityMode },
    CACHE_TAGS: new Proxy({}, { get: () => "tag" }),
    OblethApiError,
  }));
  const { setModelCapacityDiscoveryAction } = await import("./actions");
  const result = await setModelCapacityDiscoveryAction("m-1", fd);
  return { result, setModelCapacityMode };
}

function form(fields: Record<string, string>) {
  const fd = new FormData();
  for (const [k, v] of Object.entries(fields)) fd.set(k, v);
  return fd;
}

describe("saving discovered capacity settings", () => {
  it("sends the mode with every field, blanks as null", async () => {
    const { result, setModelCapacityMode } = await discoveryAction(
      form({
        capacity_source: "kubernetes",
        capacity_namespace: " inference ",
        capacity_service: "",
        per_replica_max_in_flight: "8",
        capacity_headroom: "1.25",
      }),
    );
    expect(result).toEqual({ ok: true });
    expect(setModelCapacityMode.mock.calls[0][0]).toBe("m-1");
    expect(setModelCapacityMode.mock.calls[0][1]).toBe("discovered");
    expect(setModelCapacityMode.mock.calls[0][2]).toEqual({
      capacity_source: "kubernetes",
      capacity_namespace: "inference",
      capacity_service: null,
      per_replica_max_in_flight: 8,
      capacity_headroom: 1.25,
    });
  });

  it("sends a Service name as given", async () => {
    const { setModelCapacityMode } = await discoveryAction(
      form({ capacity_source: "kubernetes", capacity_service: " my-model ", per_replica_max_in_flight: "4" }),
    );
    expect(setModelCapacityMode.mock.calls[0][2]).toMatchObject({ capacity_service: "my-model" });
  });

  it("lets the endpoints source leave the per-replica value to the endpoints", async () => {
    const { result, setModelCapacityMode } = await discoveryAction(form({ capacity_source: "endpoints" }));
    expect(result).toEqual({ ok: true });
    expect(setModelCapacityMode.mock.calls[0][2]).toMatchObject({ per_replica_max_in_flight: null });
  });

  it("defaults headroom to 1 and parses the per-replica value", async () => {
    const { setModelCapacityMode } = await discoveryAction(
      form({ capacity_source: "endpoints", per_replica_max_in_flight: "8" }),
    );
    expect(setModelCapacityMode.mock.calls[0][2]).toMatchObject({
      per_replica_max_in_flight: 8,
      capacity_headroom: 1,
    });
  });

  it.each([
    [{ capacity_source: "prometheus" }, "capacity source"],
    [{ capacity_source: "kubernetes" }, "per-replica concurrency is required"],
    [{ capacity_source: "kubernetes", per_replica_max_in_flight: "" }, "--max-num-seqs"],
    [
      { capacity_source: "kubernetes", per_replica_max_in_flight: "8", capacity_service: "app=my-model" },
      "service name",
    ],
    [{ capacity_source: "kubernetes", per_replica_max_in_flight: "8", capacity_service: "My-Model" }, "service name"],
    [{ capacity_source: "endpoints", per_replica_max_in_flight: "0" }, "at least 1"],
    [{ capacity_source: "endpoints", per_replica_max_in_flight: "2.5" }, "whole number"],
    [{ capacity_source: "endpoints", capacity_headroom: "0" }, "above 0"],
    [{ capacity_source: "endpoints", capacity_headroom: "11" }, "at most 10"],
  ])("refuses %o before calling the gateway", async (fields, message) => {
    const { result, setModelCapacityMode } = await discoveryAction(form(fields));
    expect(result.ok).toBe(false);
    expect(result.ok ? "" : result.error.toLowerCase()).toContain(message);
    expect(setModelCapacityMode).not.toHaveBeenCalled();
  });

  it("surfaces the gateway's refusal", async () => {
    const { result } = await discoveryAction(
      form({ capacity_source: "kubernetes", capacity_namespace: "kube-system", per_replica_max_in_flight: "8" }),
      new Error("capacity_namespace `kube-system` is not in OBLETH_CAPACITY_DISCOVERY_NAMESPACES"),
    );
    expect(result).toEqual({
      ok: false,
      error: "capacity_namespace `kube-system` is not in OBLETH_CAPACITY_DISCOVERY_NAMESPACES",
    });
  });
});
