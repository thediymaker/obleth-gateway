import { describe, expect, it } from "vitest";
import { cleanNamedNetworks, looksLikeNetwork, namedNetworksProblem, parseNamedNetworksField } from "@/lib/named-networks";

describe("named networks", () => {
  it("accepts addresses and ranges, v4 and v6", () => {
    for (const ok of ["10.0.0.0/8", "203.0.113.7", "2001:db8::/48", "::1", "fd00::/8"]) expect(looksLikeNetwork(ok), ok).toBe(true);
    for (const bad of ["", "10.0.0/8", "10.0.0.256/8", "10.0.0.0/33", "campus", "2001:db8::/129", "1::2::3", "10.0.0.0/8/1"]) expect(looksLikeNetwork(bad), bad).toBe(false);
  });

  it("drops blank rows and says what is wrong with the rest", () => {
    expect(cleanNamedNetworks([{ network: " 10.0.0.0/8 ", name: " Campus " }, { network: "", name: " " }])).toEqual([{ network: "10.0.0.0/8", name: "Campus" }]);
    expect(namedNetworksProblem([{ network: "10.0.0.0/8", name: "Campus" }])).toBeNull();
    expect(namedNetworksProblem([{ network: "campus", name: "x" }])).toContain("not an address");
    expect(namedNetworksProblem([{ network: "", name: "VPN" }])).toBe("VPN needs a range.");
    expect(namedNetworksProblem([{ network: "10.0.0.0/8", name: "" }])).toBe("10.0.0.0/8 needs a name.");
    expect(namedNetworksProblem([{ network: "10.0.0.0/8", name: "a" }, { network: "10.0.0.0/8", name: "b" }])).toContain("twice");
  });

  it("reads the editor's field", () => {
    expect(parseNamedNetworksField('[{"network":"10.0.0.0/8","name":"Campus","extra":1}]')).toEqual([{ network: "10.0.0.0/8", name: "Campus" }]);
    expect(parseNamedNetworksField("nope")).toBeNull();
    expect(parseNamedNetworksField(null)).toBeNull();
  });
});
