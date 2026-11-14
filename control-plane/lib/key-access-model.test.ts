import { describe, expect, it } from "vitest";
import { countryLabel, filterKeyAccess, keyAccessRows, keyAccessSummary, refusalRows } from "@/lib/key-access-model";
import type { KeyRejectionAgg, KeySource } from "@/lib/obleth";

const SINCE = 1_000_000;

const src = (over: Partial<KeySource>): KeySource => ({
  key_id: "k1",
  key_name: "lab",
  tenant_id: "t1",
  tenant_name: "genomics",
  network: "10.1.2.0/24",
  client_country: "US",
  requests: 10,
  distinct_ips: 1,
  last_seen_ms: SINCE + 100,
  first_seen_ms: SINCE - 50_000,
  network_first_seen_ms: SINCE - 50_000,
  ...over,
});

const rej = (over: Partial<KeyRejectionAgg>): KeyRejectionAgg => ({
  key_id: "k1",
  key_name: "lab",
  tenant_id: "t1",
  tenant_name: "genomics",
  reason: "ip_not_allowed",
  client_ip: "203.0.113.9",
  client_country: "DE",
  count: 4,
  first_seen_ms: SINCE + 10,
  last_seen_ms: SINCE + 20,
  sample_user_agent: "curl/8",
  ...over,
});

describe("key access rows", () => {
  it("groups networks under their key, busiest first, and marks what is new in the window", () => {
    const rows = keyAccessRows(
      [
        src({}),
        src({ network: "198.51.100.0/24", client_country: "de", requests: 30, distinct_ips: 2, first_seen_ms: SINCE + 5, network_first_seen_ms: SINCE + 5, last_seen_ms: SINCE + 900 }),
        src({ key_id: "k2", key_name: "ci", requests: 5 }),
      ],
      [rej({}), rej({ key_id: "k3", count: 2 })],
      SINCE,
    );
    expect(rows.map((r) => r.keyId)).toEqual(["k1", "k2"]);
    expect(rows[0]).toMatchObject({
      requests: 40,
      distinctIps: 3,
      lastSeenMs: SINCE + 900,
      countries: ["DE", "US"],
      newCountries: ["DE"],
      newNetworks: 1,
      refused: 4,
    });
    expect(rows[0].places.map((p) => p.network)).toEqual(["198.51.100.0/24", "10.1.2.0/24"]);
    expect(rows[0].places[0]).toMatchObject({ country: "DE", newCountry: true, newNetwork: true });
  });

  it("never calls an unknown country new", () => {
    const [row] = keyAccessRows([src({ client_country: "", first_seen_ms: null })], [], SINCE);
    expect(row.countries).toEqual([]);
    expect(row.newCountries).toEqual([]);
  });

  it("sums the window for the page's tiles", () => {
    const rejections = [rej({}), rej({ key_id: "k3", count: 2 })];
    const rows = keyAccessRows([src({}), src({ network: "198.51.100.0/24", client_country: "DE", first_seen_ms: SINCE + 1 })], rejections, SINCE);
    expect(keyAccessSummary(rows, rejections)).toEqual({ keys: 1, networks: 2, countries: 2, keysInNewCountry: 1, refused: 6, refusedKeys: 2 });
  });
});

describe("finding a key", () => {
  const rows = keyAccessRows(
    [
      src({}),
      src({ key_id: "k2", key_name: "nightly-eval", tenant_name: "physics", network: "203.0.113.0/24", client_country: "FR", first_seen_ms: SINCE + 1, requests: 2, last_seen_ms: SINCE + 5_000 }),
    ],
    [],
    SINCE,
  );

  it("searches key, tenant, network and country", () => {
    expect(filterKeyAccess(rows, "nightly", "requests", false).map((r) => r.keyId)).toEqual(["k2"]);
    expect(filterKeyAccess(rows, "physics", "requests", false).map((r) => r.keyId)).toEqual(["k2"]);
    expect(filterKeyAccess(rows, "10.1.2", "requests", false).map((r) => r.keyId)).toEqual(["k1"]);
    expect(filterKeyAccess(rows, "fr", "requests", false).map((r) => r.keyId)).toEqual(["k2"]);
  });

  it("can show only keys with something new, and sort by what's new or most recent", () => {
    expect(filterKeyAccess(rows, "", "requests", true).map((r) => r.keyId)).toEqual(["k2"]);
    expect(filterKeyAccess(rows, "", "requests", false).map((r) => r.keyId)).toEqual(["k1", "k2"]);
    expect(filterKeyAccess(rows, "", "recent", false).map((r) => r.keyId)).toEqual(["k2", "k1"]);
    expect(filterKeyAccess(rows, "", "new", false).map((r) => r.keyId)).toEqual(["k2", "k1"]);
  });
});

describe("refusals and countries", () => {
  it("lists refusals most recent first", () => {
    expect(refusalRows([rej({ last_seen_ms: 1 }), rej({ last_seen_ms: 9, client_ip: "x" })])[0].client_ip).toBe("x");
  });

  it("names countries, and says Unknown for none", () => {
    expect(countryLabel("US")).toBe("United States");
    expect(countryLabel("")).toBe("Unknown");
  });
});
