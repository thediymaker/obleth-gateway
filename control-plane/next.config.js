const path = require("path");

// Static security headers. Content-Security-Policy is deliberately absent
// here: it carries a per-request script nonce and is set in proxy.ts
// (see lib/csp.ts).
const securityHeaders = [
  { key: "X-Frame-Options", value: "DENY" },
  { key: "X-Content-Type-Options", value: "nosniff" },
  { key: "Referrer-Policy", value: "strict-origin-when-cross-origin" },
  { key: "X-DNS-Prefetch-Control", value: "off" },
  {
    key: "Permissions-Policy",
    value: "camera=(), microphone=(), geolocation=()",
  },
  {
    key: "Strict-Transport-Security",
    value: "max-age=63072000; includeSubDomains; preload",
  },
];

/** @type {import('next').NextConfig} */
const nextConfig = {
  output: "standalone",
  reactStrictMode: true,
  // pin tracing root to this app (avoids picking up stray parent lockfiles)
  outputFileTracingRoot: path.join(__dirname),
  experimental: {
    serverActions: {
      // Config-backup restores upload the whole backup file through a server
      // action; large key fleets exceed the 1 MB default.
      bodySizeLimit: "64mb",
    },
  },
  async headers() {
    return [{ source: "/:path*", headers: securityHeaders }];
  },
};

module.exports = nextConfig;
