import { describe, it, expect, beforeEach } from "vitest";
import {
  registerActivity, getActivity, listActivities, __clearActivities,
} from "./registry";
import type { Activity } from "./types";

const fake: Activity = {
  id: "demo",
  label: "Demo",
  blurb: "A demo activity",
  kind: "run",
  toolName: "demo_tool",
  resultType: "demo_result",
  steps: [{ type: "model", label: "Model" }],
};

describe("activity registry", () => {
  beforeEach(() => __clearActivities());

  it("registers and retrieves by id", () => {
    registerActivity(fake);
    expect(getActivity("demo")).toEqual(fake);
  });

  it("lists in registration order", () => {
    registerActivity(fake);
    registerActivity({ ...fake, id: "demo2", label: "Demo 2" });
    expect(listActivities().map((a) => a.id)).toEqual(["demo", "demo2"]);
  });

  it("returns undefined for unknown id", () => {
    expect(getActivity("nope")).toBeUndefined();
  });
});
