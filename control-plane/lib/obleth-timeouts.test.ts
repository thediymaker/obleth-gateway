// @vitest-environment node
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { API_TIMEOUT_MS, obleth, OblethApiError } from "./obleth";
import { safe } from "./safe";

/** A gateway that takes the call and never answers, until the call is aborted. */
function stalledGateway() {
  vi.stubGlobal(
    "fetch",
    vi.fn(
      (_url: string | URL, init?: RequestInit) =>
        new Promise<Response>((_, reject) => {
          init?.signal?.addEventListener("abort", () => reject(init.signal?.reason));
        }),
    ),
  );
}

describe("Management API time limits", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.stubEnv("OBLETH_ADMIN_TOKEN", "test-token");
    vi.spyOn(console, "error").mockImplementation(() => {});
    // AbortSignal.timeout runs on Node's own timers; put it on the fake clock.
    vi.spyOn(AbortSignal, "timeout").mockImplementation((ms) => {
      const c = new AbortController();
      setTimeout(() => c.abort(new DOMException("timed out", "TimeoutError")), ms);
      return c.signal;
    });
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.unstubAllEnvs();
    vi.restoreAllMocks();
  });

  it("gives up on a read the gateway does not answer", async () => {
    stalledGateway();
    const call = obleth.stats();
    const failed = expect(call).rejects.toSatisfy(
      (e) => e instanceof OblethApiError && e.status === 504 && /did not answer in 35 s/.test(e.message),
    );
    await vi.advanceTimersByTimeAsync(API_TIMEOUT_MS.read);
    await failed;
  });

  it("lets safe() fall back when the gateway stalls", async () => {
    stalledGateway();
    const read = safe(obleth.stats(), null);
    await vi.advanceTimersByTimeAsync(API_TIMEOUT_MS.read);
    await expect(read).resolves.toBeNull();
  });

  it("waits longer for writes and long operations", async () => {
    stalledGateway();
    let settled = false;
    const restore = obleth.restoreBackup("{}").catch(() => {
      settled = true;
    });
    await vi.advanceTimersByTimeAsync(API_TIMEOUT_MS.write);
    expect(settled).toBe(false);
    await vi.advanceTimersByTimeAsync(API_TIMEOUT_MS.long - API_TIMEOUT_MS.write);
    await restore;
    expect(settled).toBe(true);
  });
});
