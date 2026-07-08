import { FlaskConical } from "lucide-react";
import type { Activity } from "./types";

export const testCapabilitiesActivity: Activity = {
  id: "test_capabilities",
  label: "Test a model's capabilities",
  blurb: "Fire its boons — tools, JSON, vision — and see the trace",
  icon: FlaskConical,
  kind: "run",
  toolName: "test_capabilities",
  resultType: "capability_result",
  steps: [
    { type: "model", label: "Model" },
    { type: "checklist", key: "tests", label: "What to test", optionsFrom: "boons" },
    { type: "image", key: "image", label: "Test image", onlyWhen: { key: "tests", includes: "vision" } },
  ],
};
