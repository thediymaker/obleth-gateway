import { SignJWT, jwtVerify } from "jose";
import { cookies } from "next/headers";

export const SESSION_COOKIE = "obleth_session";
const SESSION_TTL = "8h";

export interface SessionUser {
  username: string;
}

function secret() {
  const s = process.env.DASHBOARD_SESSION_SECRET;
  if (!s || s.length < 32) {
    throw new Error(
      "DASHBOARD_SESSION_SECRET is not set or is too short. Set it to a random value of at least 32 characters (e.g. `openssl rand -base64 48`).",
    );
  }
  return new TextEncoder().encode(s);
}

export async function createSession(user: SessionUser) {
  const token = await new SignJWT({ username: user.username })
    .setProtectedHeader({ alg: "HS256" })
    .setIssuedAt()
    .setExpirationTime(SESSION_TTL)
    .sign(secret());

  const jar = await cookies();
  jar.set(SESSION_COOKIE, token, {
    httpOnly: true,
    sameSite: "lax",
    secure: process.env.NODE_ENV === "production",
    path: "/",
  });
}

export async function destroySession() {
  const jar = await cookies();
  jar.delete(SESSION_COOKIE);
}

export async function getSession(): Promise<SessionUser | null> {
  const jar = await cookies();
  const token = jar.get(SESSION_COOKIE)?.value;
  if (!token) return null;
  try {
    const { payload } = await jwtVerify(token, secret());
    const username = payload.username;
    if (typeof username !== "string") return null;
    return { username };
  } catch {
    return null;
  }
}

/**
 * Authorize the current request. Server Actions are POST routes that the Next.js
 * proxy does not reliably cover, so every privileged action must call this to
 * fail closed when the caller is unauthenticated.
 */
export async function requireSession(): Promise<SessionUser> {
  const session = await getSession();
  if (!session) {
    throw new Error("Unauthorized");
  }
  return session;
}
