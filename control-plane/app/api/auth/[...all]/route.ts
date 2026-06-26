import { auth } from "@/lib/auth/better-auth";
import { toNextJsHandler } from "better-auth/next-js";

// Defer auth.handler access to first request so `next build` (which imports
// route modules without DATABASE_URL) does not construct the better-auth
// instance at module load. The lazy Proxy in better-auth.ts prevents
// import-time construction, but a module-scope property read of `auth.handler`
// would still trigger getAuth() → getDb() and throw at build time.
let handlers: ReturnType<typeof toNextJsHandler> | null = null;
function h() {
  return (handlers ??= toNextJsHandler(auth.handler));
}

export const GET = (req: Request) => h().GET(req);
export const POST = (req: Request) => h().POST(req);
