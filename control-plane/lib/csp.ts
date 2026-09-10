// Content Security Policy for every dashboard document, built per request in
// proxy.ts. Scripts are allowed by nonce: Next.js reads the nonce from the
// request's Content-Security-Policy header and stamps it on every script tag
// it emits, and 'strict-dynamic' extends trust to the chunks those scripts
// load. Nothing else — no inline script without the nonce, no eval — runs in
// production. Development keeps 'unsafe-eval' for React Refresh and source
// maps only.
//
// Styles stay on 'unsafe-inline': Next's font loader, Recharts, and shadcn
// primitives write style attributes that a nonce cannot cover.

export interface CspOptions {
  development?: boolean;
}

export function contentSecurityPolicy(nonce: string, opts: CspOptions = {}): string {
  const development = opts.development ?? process.env.NODE_ENV === "development";
  return [
    "default-src 'self'",
    `script-src 'self' 'nonce-${nonce}' 'strict-dynamic'${development ? " 'unsafe-eval'" : ""}`,
    "style-src 'self' 'unsafe-inline'",
    "img-src 'self' data: blob:",
    "font-src 'self' data:",
    "connect-src 'self'",
    "frame-ancestors 'none'",
    "base-uri 'self'",
    "form-action 'self'",
    "object-src 'none'",
  ].join("; ");
}

/** A fresh base64 nonce per document; 128 bits of randomness. */
export function generateNonce(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return btoa(String.fromCharCode(...bytes));
}
