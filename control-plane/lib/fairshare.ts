// Canonical starvation predicate shared by the overview and fairshare pages so
// both agree on which tenants count as waiting below their fair share.
export interface FairshareTenantSlots {
  queued: number;
  in_flight: number;
  expected_slots: number;
}

export function isWaitingBelowShare(t: FairshareTenantSlots): boolean {
  return t.queued > 0 && t.in_flight < Math.floor(t.expected_slots);
}
