import { describe, expect, it } from "vitest";
import {
  cleanNamedNetworks,
  networksHolding,
  splitAllowedNetworks,
  looksLikeNetwork,
  namedNetworkProblems,
  namedNetworksProblem,
  oursFirst,
  parseNamedNetworksField,
  rangeCount,
  rangeLines,
} from "@/lib/named-networks";

const g = (name: string, ranges: string[], ours = false) => ({ name, ranges, ours });

describe("named networks", () => {
  it("accepts addresses and ranges, v4 and v6", () => {
    for (const ok of ["10.0.0.0/8", "203.0.113.7", "2001:db8::/48", "::1", "fd00::/8"]) expect(looksLikeNetwork(ok), ok).toBe(true);
    for (const bad of ["", "10.0.0/8", "10.0.0.256/8", "10.0.0.0/33", "campus", "2001:db8::/129", "1::2::3", "10.0.0.0/8/1"]) expect(looksLikeNetwork(bad), bad).toBe(false);
  });

  it("reads ranges one per line", () => {
    expect(rangeLines(" 10.0.0.0/8 \n\n203.0.113.0/24, 2001:db8::/48\n")).toEqual(["10.0.0.0/8", "203.0.113.0/24", "2001:db8::/48"]);
  });

  it("drops blank groups and says what is wrong with the rest", () => {
    expect(cleanNamedNetworks([g(" Campus ", [" 10.0.0.0/8 ", ""], true), g(" ", [" "])])).toEqual([g("Campus", ["10.0.0.0/8"], true)]);
    expect(namedNetworksProblem([g("Campus", ["10.0.0.0/8", "203.0.113.0/24"])])).toBeNull();
    expect(namedNetworksProblem([g("x", ["campus"])])).toContain("not an address");
    expect(namedNetworksProblem([g("VPN", [])])).toBe("VPN needs at least one address or range.");
    expect(namedNetworksProblem([g("", ["10.0.0.0/8"])])).toBe("10.0.0.0/8 needs a name.");
    expect(namedNetworksProblem([g("Campus", ["10.0.0.0/8"]), g("campus", ["10.1.0.0/16"])])).toBe("Another network is already called Campus.");
  });

  it("names both networks when a range is in two, per group", () => {
    const problems = namedNetworkProblems([g("Campus", ["10.0.0.0/8"]), g("", []), g("Cluster", ["10.42.0.0/16", "10.0.0.0/8"])]);
    expect(problems[0]).toBeNull();
    expect(problems[1]).toBeNull();
    expect(problems[2]).toBe("10.0.0.0/8 is in both Campus and Cluster. A range can belong to one network only.");
  });

  it("counts ranges and offers our own networks first", () => {
    const groups = [g("Partner", ["198.51.100.0/24"]), g("Campus", ["10.0.0.0/8", "203.0.113.0/24"], true)];
    expect(rangeCount(groups)).toBe(3);
    expect(oursFirst(groups).map((x) => x.name)).toEqual(["Campus", "Partner"]);
  });

  it("reads the editor's field, groups and older one-range rows alike", () => {
    expect(parseNamedNetworksField('[{"name":"Campus","ranges":["10.0.0.0/8"],"ours":true,"extra":1},{"network":"10.42.0.0/16","name":"Pods"},7]')).toEqual([
      g("Campus", ["10.0.0.0/8"], true),
      g("Pods", ["10.42.0.0/16"]),
    ]);
    expect(parseNamedNetworksField("nope")).toBeNull();
    expect(parseNamedNetworksField(null)).toBeNull();
  });

  it("reads a key's list as the networks it holds whole and the rest", () => {
    const groups = [
      { name: "Campus", ranges: ["10.0.0.0/8", "2001:db8::/48"], ours: true },
      { name: "Lab", ranges: ["198.51.100.0/24"], ours: false },
    ];
    expect(splitAllowedNetworks(["192.0.2.7", " 2001:DB8::/48", "10.0.0.0/8", "198.51.100.0/25"], groups)).toEqual({
      names: ["Campus"],
      rest: ["192.0.2.7", "198.51.100.0/25"],
    });
    expect(splitAllowedNetworks([], groups)).toEqual({ names: [], rest: [] });
  });

  it("names the other networks that hold every range of one", () => {
    const groups = [
      { name: "Sol", ranges: ["10.139.121.0/24", "2001:db8:0:1::/64"], ours: true },
      { name: "Research", ranges: ["10.139.0.0/16", "2001:db8::/48"], ours: true },
      { name: "Campus", ranges: ["10.0.0.0/8", "2001:db8::/32"], ours: true },
      { name: "Lab", ranges: ["10.139.121.0/25"], ours: false },
      { name: "Mapped", ranges: ["::ffff:10.139.121.0/120"], ours: false },
    ];
    expect(networksHolding(groups[0]!, groups)).toEqual(["Research", "Campus"]);
    expect(networksHolding(groups[3]!, groups)).toEqual(["Sol", "Research", "Campus"]);
    expect(networksHolding(groups[2]!, groups)).toEqual([]);
    expect(networksHolding(groups[4]!, groups)).toEqual([]);
    expect(networksHolding({ name: "Bad", ranges: ["nope"], ours: false }, groups)).toEqual([]);
  });
});
