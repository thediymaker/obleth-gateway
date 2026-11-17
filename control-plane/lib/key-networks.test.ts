import { describe, expect, it } from "vitest";
import { countriesText, countryCodes, countryName, networkLines, networksSummary, networksText } from "./key-networks";

describe("key networks", () => {
  it("reads one entry per line or comma, dropping blanks", () => {
    expect(networkLines(" 203.0.113.0/24\n\n2001:db8::/48 , 198.51.100.7\r\n")).toEqual([
      "203.0.113.0/24",
      "2001:db8::/48",
      "198.51.100.7",
    ]);
    expect(networkLines("")).toEqual([]);
    expect(networkLines(null)).toEqual([]);
  });

  it("shows the list one per line and sums it up in words", () => {
    expect(networksText(["10.0.0.0/8", "192.0.2.1/32"])).toBe("10.0.0.0/8\n192.0.2.1/32");
    expect(networksText(undefined)).toBe("");
    expect(networksSummary([])).toBe("Any network");
    expect(networksSummary(undefined)).toBe("Any network");
    expect(networksSummary(["10.0.0.0/8"])).toBe("Only 10.0.0.0/8");
    expect(networksSummary(["10.0.0.0/8", "192.0.2.0/24"])).toBe("Only 2 networks");
  });

  it("sums up networks and countries together", () => {
    expect(networksSummary([], [])).toBe("Any network");
    expect(networksSummary([], ["US"])).toBe("Only US");
    expect(networksSummary(undefined, ["US", "CA"])).toBe("Only 2 countries");
    expect(networksSummary(["10.0.0.0/8"], ["US"])).toBe("Only 10.0.0.0/8 or US");
    expect(networksSummary(["10.0.0.0/8", "192.0.2.0/24"], ["US", "CA"])).toBe("Only 2 networks or 2 countries");
  });

  it("reads country codes split by commas or spaces, upper-cased and once each", () => {
    expect(countryCodes(" us, ca\nGB  us ")).toEqual(["US", "CA", "GB"]);
    expect(countryCodes("")).toEqual([]);
    expect(countryCodes(null)).toEqual([]);
    expect(countriesText(["US", "CA"])).toBe("US, CA");
    expect(countriesText(undefined)).toBe("");
  });

  it("names a country by its code, and nothing that isn't one", () => {
    expect(countryName("US")).toBe("United States");
    expect(countryName("de")).toBe("Germany");
    for (const bad of ["XX", "USA", "U1", "", "QQ"]) expect(countryName(bad)).toBeNull();
  });
});
