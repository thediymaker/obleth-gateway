// Server-only version identity for the control plane, plus the update check
// against GitHub releases. Versions are lockstep across the repo: one vX.Y.Z
// tag releases the gateway, this dashboard, and the Helm chart together.
import pkg from "../package.json";

export const CONTROL_PLANE_VERSION: string = pkg.version;

/**
 * Git commit this image was built from; null for local runs (Docker sets the
 * env to "" when the build-arg is omitted, so empty also means absent).
 */
export const CONTROL_PLANE_SHA: string | null =
  process.env.OBLETH_BUILD_SHA || null;

const RELEASES_URL =
  "https://api.github.com/repos/thediymaker/obleth-gateway/releases/latest";

export interface LatestRelease {
  tag: string;
  url: string;
  published_at: string;
}

/**
 * Latest published GitHub release, or null when the check is unavailable
 * (no release yet, rate-limited, or an air-gapped install). Cached for an
 * hour via the Data Cache so the unauthenticated rate limit is never an
 * issue.
 */
export async function fetchLatestRelease(): Promise<LatestRelease | null> {
  try {
    const res = await fetch(RELEASES_URL, {
      headers: { Accept: "application/vnd.github+json" },
      next: { revalidate: 3600 },
    });
    if (!res.ok) return null;
    const release = await res.json();
    if (typeof release?.tag_name !== "string") return null;
    return {
      tag: release.tag_name,
      url: release.html_url,
      published_at: release.published_at,
    };
  } catch {
    return null;
  }
}

/**
 * True when `latest` (e.g. "v0.3.0") is newer than `installed` ("0.2.0").
 * Lockstep release tags are always plain vX.Y.Z, so a numeric triplet
 * compare is sufficient; prerelease suffixes are ignored.
 */
export function isNewer(latest: string, installed: string): boolean {
  const parse = (v: string) =>
    v.replace(/^v/, "").split("-")[0].split(".").map(Number);
  const a = parse(latest);
  const b = parse(installed);
  for (let i = 0; i < 3; i++) {
    const x = a[i] ?? 0;
    const y = b[i] ?? 0;
    if (Number.isNaN(x) || Number.isNaN(y)) return false;
    if (x !== y) return x > y;
  }
  return false;
}
