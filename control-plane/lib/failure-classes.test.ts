import { describe, expect, it } from "vitest";
import { classLabel, classSide, ERROR_CLASS_IDS, failureReason, parseClasses, parseFault } from "./failure-classes";

describe("failure classes", () => {
  it("names every class in plain words, on one side or the other", () => {
    expect(ERROR_CLASS_IDS).toHaveLength(9);
    expect(classLabel("client_input")).toBe("Request refused");
    expect(classLabel("network")).toBe("Couldn't reach the model");
    expect(ERROR_CLASS_IDS.filter((c) => classSide(c) === "platform")).toEqual(["capacity", "upstream_timeout", "upstream_error", "network", "gateway"]);
    expect(classSide("limit")).toBe("client");
  });

  it("reads a success, and a class it does not know, without inventing a side", () => {
    expect(classLabel("")).toBe("");
    expect(classLabel(undefined)).toBe("");
    expect(classLabel("brand_new_kind")).toBe("brand new kind");
    expect(classSide("brand_new_kind")).toBeNull();
    expect(classSide("")).toBeNull();
  });

  it("gives the reason as recorded, and only the status for rows from before reasons", () => {
    expect(failureReason({ status_code: 400, error_reason: "This model's maximum context length is 262144 tokens." })).toBe("This model's maximum context length is 262144 tokens.");
    expect(failureReason({ status_code: 400, error_reason: "" })).toBe("HTTP 400");
    expect(failureReason({ status_code: 502 })).toBe("HTTP 502");
  });

  it("reads classes and sides from a search", () => {
    expect(parseClasses("capacity")).toEqual(["capacity"]);
    expect(parseClasses("Upstream-Timeout,network,network")).toEqual(["upstream_timeout", "network"]);
    expect(parseClasses("capacity,nope")).toBeNull();
    expect(parseClasses("")).toBeNull();
    expect(parseFault("platform")).toBe("platform");
    expect(parseFault("Caller")).toBe("client");
    expect(parseFault("client")).toBe("client");
    expect(parseFault("someone")).toBeNull();
  });
});
