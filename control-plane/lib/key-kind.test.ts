import { describe, expect, it } from "vitest";
import { describeIdentityKey } from "./key-kind";

describe("describeIdentityKey", () => {
  it("is not an identity for secret keys", () => {
    expect(describeIdentityKey({ kind: "secret", identity_issuer: null, identity_subject: null })).toEqual({
      isIdentity: false,
      issuerHost: "",
      subject: "",
    });
  });

  it("extracts the issuer host and subject", () => {
    expect(
      describeIdentityKey({
        kind: "identity",
        identity_issuer: "https://idp.example.com/realms/main",
        identity_subject: "alice",
      }),
    ).toEqual({ isIdentity: true, issuerHost: "idp.example.com", subject: "alice" });
  });

  it("falls back to the raw issuer when it is not a URL", () => {
    expect(
      describeIdentityKey({ kind: "identity", identity_issuer: "not a url", identity_subject: "alice" }).issuerHost,
    ).toBe("not a url");
  });
});
