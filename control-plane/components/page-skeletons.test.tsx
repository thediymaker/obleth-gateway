import { act } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, it } from "vitest";
import { DashboardSkeleton, DeploymentSkeleton, DeploymentsSkeleton, FairshareSkeleton, GroupsSkeleton, KeysSkeleton, LogsSkeleton, ModelSkeleton, ModelsSkeleton, OverviewSkeleton, PlaygroundSkeleton, ReportsSkeleton, TenantSkeleton, TenantsSkeleton, UsersSkeleton } from "./page-skeletons";

describe("page skeletons", () => {
  it.each([
    ["Loading the overview", OverviewSkeleton],
    ["Loading fairshare", FairshareSkeleton],
    ["Loading groups and weights", GroupsSkeleton],
    ["Loading the playground", PlaygroundSkeleton],
    ["Loading models", ModelsSkeleton],
    ["Loading the model", ModelSkeleton],
    ["Loading request logs", LogsSkeleton],
    ["Loading reports", ReportsSkeleton],
    ["Loading tenants", TenantsSkeleton],
    ["Loading the tenant", TenantSkeleton],
    ["Loading API keys", KeysSkeleton],
    ["Loading users", UsersSkeleton],
    ["Loading deployments", DeploymentsSkeleton],
    ["Loading the deployment", DeploymentSkeleton],
    ["Loading page", DashboardSkeleton],
  ])("announces %s once, and hides the placeholder blocks from assistive tech", (label, Component) => {
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
    const host = document.createElement("div");
    const root = createRoot(host);
    act(() => root.render(<Component />));
    const status = host.querySelector('[role="status"]')!;
    expect(status.getAttribute("aria-busy")).toBe("true");
    expect(status.getAttribute("aria-label")).toBe(label);
    const blocks = host.querySelectorAll(".skeleton");
    expect(blocks.length).toBeGreaterThan(4);
    blocks.forEach((b) => expect(b.closest('[aria-hidden="true"]')).not.toBeNull());
    act(() => root.unmount());
  });
});
