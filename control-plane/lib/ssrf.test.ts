import { afterEach, describe, expect, it, vi } from "vitest";
import { isBlockedAddress } from "./ssrf";

afterEach(() => {
  vi.resetModules();
  vi.doUnmock("node:dns/promises");
});

describe("isBlockedAddress", () => {
  it.each([
    "169.254.169.254",
    "169.254.0.1",
    "0.0.0.0",
    "100.100.100.200",
    "fe80::1",
    "febf::1",
    "[fe80::1%eth0]",
    "::",
    "::ffff:169.254.169.254",
    "::ffff:a9fe:a9fe",
    "::ffff:0.0.0.0",
    "::ffff:100.100.100.200",
    "0:0:0:0:0:ffff:6464:64c8",
    "::169.254.169.254",
    "::a9fe:a9fe",
    "::100.100.100.200",
    "64:ff9b::169.254.169.254",
    "64:ff9b::a9fe:a9fe",
    "64:ff9b:0:0:0:0:6464:64c8",
  ])("blocks %s", (address) => {
    expect(isBlockedAddress(address)).toBe(true);
  });

  it.each([
    "10.0.0.5",
    "192.168.1.20",
    "172.16.0.1",
    "127.0.0.1",
    "::1",
    "8.8.8.8",
    "100.100.100.201",
    "fec0::1",
    "2001:db8::1",
    "::ffff:10.0.0.5",
    "::ffff:127.0.0.1",
    "::10.0.0.5",
    "64:ff9b::10.0.0.5",
    "64:ff9b::808:808",
  ])("allows %s (local-first policy)", (address) => {
    expect(isBlockedAddress(address)).toBe(false);
  });

  it("treats a non-IP as blocked", () => {
    expect(isBlockedAddress("not-an-ip")).toBe(true);
  });
});

describe("blockedHostReason", () => {
  it("refuses a name when any resolved address is blocked", async () => {
    const lookup = async () => [{ address: "10.0.0.5" }, { address: "169.254.169.254" }];
    vi.doMock("node:dns/promises", () => ({ lookup, default: { lookup } }));
    const { blockedHostReason } = await import("./ssrf");
    expect(await blockedHostReason("provider.example")).toMatch(/not allowed/);
  });

  it("allows a name that resolves only to permitted addresses", async () => {
    const lookup = async () => [{ address: "10.0.0.5" }];
    vi.doMock("node:dns/promises", () => ({ lookup, default: { lookup } }));
    const { blockedHostReason } = await import("./ssrf");
    expect(await blockedHostReason("provider.example")).toBeNull();
  });
});
