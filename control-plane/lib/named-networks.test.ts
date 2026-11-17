import { describe, expect, it } from "vitest";
import {
  cleanNamedNetworks,
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
});
