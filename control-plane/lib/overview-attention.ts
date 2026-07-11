export interface OverviewAttentionItem {
  title: string;
  detail: string;
  href: "/models" | "/fairshare";
  tone: "warn" | "hot";
}

export function buildOverviewAttentionItems(input: {
  unhealthyModels: number;
  unknownModels: number;
  queuedRequests: number;
  waitingTenants: number;
  starvedTenants: number;
}): OverviewAttentionItem[] {
  const items: OverviewAttentionItem[] = [];
  if (input.unhealthyModels > 0) {
    items.push({
      title: `${input.unhealthyModels} unhealthy model route${input.unhealthyModels === 1 ? "" : "s"}`,
      detail: "Inspect endpoints and recent health checks.",
      href: "/models",
      tone: "hot",
    });
  }
  if (input.unknownModels > 0) {
    items.push({
      title: `${input.unknownModels} route${input.unknownModels === 1 ? "" : "s"} awaiting health data`,
      detail: "Confirm enabled routes can be reached.",
      href: "/models",
      tone: "warn",
    });
  }
  if (input.queuedRequests > 0) {
    const starvation = input.starvedTenants > 0 ? ` ${input.starvedTenants} below expected share.` : "";
    items.push({
      title: `${input.queuedRequests} request${input.queuedRequests === 1 ? "" : "s"} queued`,
      detail: `${input.waitingTenants} tenant${input.waitingTenants === 1 ? "" : "s"} waiting for admission.${starvation}`,
      href: "/fairshare",
      tone: input.starvedTenants > 0 ? "hot" : "warn",
    });
  }
  return items;
}
