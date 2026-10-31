import { describe, expect, it } from "vitest";
import { failureHeadline, groupLogsHref, groupsFor, seenAgo, share, trendOf, TREND_POINTS } from "./failures-model";
import { NIL_REQUEST_ID } from "./logs-model";
import type { UsageFailureGroup } from "./obleth";

const group = (over: Partial<UsageFailureGroup> = {}): UsageFailureGroup => ({
  error_class: "client_input", platform: false, tenant_id: "t-cs", tenant_name: "cs-teaching", key_id: "k-1", key_name: "canvas-tutor",
  model: "glm-5-3", status_code: 400, reason: "prompt contains at least 196609 input tokens", requests: 9000, first_ms: 0, last_ms: 0, ...over,
});

describe("the failures headline", () => {
  it("splits the failures by side", () => {
    expect(failureHeadline({ requests: 661_802, failures: 412, platform_failures: 87 }, "24h")).toBe("412 failed of 661,802 requests: 87 on the platform's side, 325 from callers");
  });

  it("says so when nothing failed, or nothing came in", () => {
    expect(failureHeadline({ requests: 1200, failures: 0, platform_failures: 0 }, "24h")).toBe("None of 1,200 requests failed");
    expect(failureHeadline({ requests: 0, failures: 0, platform_failures: 0 }, "1h")).toBe("No requests in the last hour");
  });

  it("gives small shares enough digits to read", () => {
    expect(share(87, 661_802)).toBe("0.01%");
    expect(share(1, 661_802)).toBe("< 0.01%");
    expect(share(5, 200)).toBe("2.5%");
    expect(share(0, 200)).toBe("");
  });
});

describe("a failure group", () => {
  it("links to its requests: key, model, status and class over the page's range", () => {
    expect(groupLogsHref(group(), "24h")).toBe("/logs?code=400&class=client_input&model=glm-5-3&key=k-1&window=24h");
    // Refused before a key was read: the team instead, when there is one.
    expect(groupLogsHref(group({ key_id: NIL_REQUEST_ID, error_class: "limit", status_code: 429 }), "1h")).toBe("/logs?code=429&class=limit&model=glm-5-3&team=t-cs&window=1h");
    expect(groupLogsHref(group({ key_id: NIL_REQUEST_ID, tenant_id: NIL_REQUEST_ID, model: "" }), "7d")).toBe("/logs?code=400&class=client_input&window=7d");
  });

  it("keeps one side's groups when asked", () => {
    const groups = [group(), group({ error_class: "capacity", platform: true, status_code: 503 })];
    expect(groupsFor(groups, "all")).toHaveLength(2);
    expect(groupsFor(groups, "platform").map((g) => g.error_class)).toEqual(["capacity"]);
    expect(groupsFor(groups, "client").map((g) => g.error_class)).toEqual(["client_input"]);
  });

  it("says how long ago it was seen", () => {
    const now = 10 * 86_400_000;
    expect(seenAgo(now - 12_000, now)).toBe("12 s ago");
    expect(seenAgo(now - 4 * 60_000, now)).toBe("4 min ago");
    expect(seenAgo(now - 3 * 3_600_000, now)).toBe("3 h ago");
    expect(seenAgo(now - 3 * 86_400_000, now)).toBe("3 days ago");
  });

  it("lays the histogram's buckets out as a trend, zero where a bucket was left out", () => {
    const since = 1_000_000;
    const trend = trendOf([{ bucket_ms: 1_000_000, n: 3 }, { bucket_ms: 1_000_000 + 5 * 1000, n: 2 }], since, 1000, (b) => b.n);
    expect(trend).toHaveLength(TREND_POINTS + 1);
    expect(trend.slice(0, 6)).toEqual([3, 0, 0, 0, 0, 2]);
    expect(trendOf([], since, 0, () => 1)).toEqual([]);
  });
});
