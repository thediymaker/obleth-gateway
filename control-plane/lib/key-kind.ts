import type { ApiKey } from "@/lib/obleth";

/** Display facts for a key that stands for a verified external identity. */
export function describeIdentityKey(
  key: Pick<ApiKey, "kind" | "identity_issuer" | "identity_subject">,
): { isIdentity: boolean; issuerHost: string; subject: string } {
  if (key.kind !== "identity") return { isIdentity: false, issuerHost: "", subject: "" };
  const issuer = key.identity_issuer ?? "";
  let issuerHost = issuer;
  try {
    issuerHost = new URL(issuer).host;
  } catch {
    // Not a URL; show it verbatim.
  }
  return { isIdentity: true, issuerHost, subject: key.identity_subject ?? "" };
}
