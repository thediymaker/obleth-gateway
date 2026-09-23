// @vitest-environment node
import { beforeAll, describe, expect, it } from "vitest";
import { memoryAdapter } from "better-auth/adapters/memory";

// Runs the REAL better-auth configuration against an in-memory store, so the
// plugin's own `/api/auth/admin/*` endpoints are exercised through the same
// before-hook a production request would hit.

type Row = Record<string, unknown>;
type Store = { user: Row[]; session: Row[]; account: Row[]; verification: Row[] };
type BetterAuthModule = typeof import("./better-auth");
type Auth = ReturnType<BetterAuthModule["createAuth"]>;

const PASSWORD = "supersecretpassword";

let mod: BetterAuthModule;

beforeAll(async () => {
  // The cold import of better-auth dominates the first run; pay it once here.
  mod = await import("./better-auth");
}, 30_000);

function emptyStore(): Store {
  return { user: [], session: [], account: [], verification: [] };
}

async function addUser(
  auth: Auth,
  email: string,
  fields: { role: string; status: string },
): Promise<string> {
  const ctx = await auth.$context;
  const user = await ctx.internalAdapter.createUser({ email, name: email, emailVerified: true, ...fields });
  await ctx.internalAdapter.linkAccount({
    userId: user.id,
    providerId: "credential",
    accountId: user.id,
    password: await ctx.password.hash(PASSWORD),
  });
  return user.id;
}

async function signedIn(auth: Auth, email: string): Promise<Headers> {
  const { headers } = await auth.api.signInEmail({
    body: { email, password: PASSWORD },
    returnHeaders: true,
  });
  const cookie = (headers.get("set-cookie") ?? "").split(";")[0];
  return new Headers({ cookie });
}

/** Two active admins, signed in as the first. */
async function twoAdmins() {
  const store = emptyStore();
  const auth = mod.createAuth(memoryAdapter(store), "x".repeat(32));
  const self = await addUser(auth, "self@example.com", { role: "admin", status: "active" });
  const other = await addUser(auth, "other@example.com", { role: "admin", status: "active" });
  const headers = await signedIn(auth, "self@example.com");
  return { store, auth, self, other, headers };
}

function userRow(store: Store, id: string): Row | undefined {
  return store.user.find((u) => u.id === id);
}

describe("built-in admin endpoints respect the admin-removal guard", { timeout: 30_000 }, () => {
  it("refuses set-role, ban, remove, and update-user against the caller", async () => {
    const { store, auth, self, headers } = await twoAdmins();
    const { SELF_ADMIN_REMOVAL_ERROR } = mod;

    await expect(auth.api.setRole({ body: { userId: self, role: "user" }, headers })).rejects.toThrow(
      SELF_ADMIN_REMOVAL_ERROR,
    );
    await expect(auth.api.banUser({ body: { userId: self }, headers })).rejects.toThrow(
      SELF_ADMIN_REMOVAL_ERROR,
    );
    await expect(auth.api.removeUser({ body: { userId: self }, headers })).rejects.toThrow(
      SELF_ADMIN_REMOVAL_ERROR,
    );
    await expect(
      auth.api.adminUpdateUser({ body: { userId: self, data: { status: "pending" } }, headers }),
    ).rejects.toThrow(SELF_ADMIN_REMOVAL_ERROR);
    expect(userRow(store, self)).toMatchObject({ role: "admin", status: "active" });
  });

  it("refuses set-role, ban, remove, and update-user against the last active admin", async () => {
    const { store, auth, self, other, headers } = await twoAdmins();
    const { LAST_ADMIN_REMOVAL_ERROR } = mod;
    // The caller's session survives, but a banned admin no longer counts
    // toward the admins that would remain.
    const ctx = await auth.$context;
    await ctx.internalAdapter.updateUser(self, { banned: true });

    await expect(auth.api.setRole({ body: { userId: other, role: "user" }, headers })).rejects.toThrow(
      LAST_ADMIN_REMOVAL_ERROR,
    );
    await expect(auth.api.banUser({ body: { userId: other }, headers })).rejects.toThrow(
      LAST_ADMIN_REMOVAL_ERROR,
    );
    await expect(auth.api.removeUser({ body: { userId: other }, headers })).rejects.toThrow(
      LAST_ADMIN_REMOVAL_ERROR,
    );
    await expect(
      auth.api.adminUpdateUser({ body: { userId: other, data: { role: "user" } }, headers }),
    ).rejects.toThrow(LAST_ADMIN_REMOVAL_ERROR);
    expect(userRow(store, other)).toMatchObject({ role: "admin", status: "active" });
    expect(userRow(store, other)?.banned).not.toBe(true);
  });

  it("allows demoting another admin while an active admin remains", async () => {
    const { store, auth, other, headers } = await twoAdmins();
    await auth.api.setRole({ body: { userId: other, role: "user" }, headers });
    expect(userRow(store, other)).toMatchObject({ role: "user" });
  });

  it("finds the remaining admin past the adapter's first page of active users", async () => {
    const store = emptyStore();
    const auth = mod.createAuth(memoryAdapter(store), "x".repeat(32));
    const ctx = await auth.$context;
    const target = await addUser(auth, "target@example.com", { role: "admin", status: "active" });
    for (let i = 0; i < 150; i++) {
      await ctx.internalAdapter.createUser({
        email: `user${i}@example.com`,
        name: `user${i}`,
        emailVerified: true,
        role: "user",
        status: "active",
      });
    }
    await addUser(auth, "caller@example.com", { role: "admin", status: "active" });
    const headers = await signedIn(auth, "caller@example.com");

    await auth.api.setRole({ body: { userId: target, role: "user" }, headers });
    expect(userRow(store, target)).toMatchObject({ role: "user" });
  });

  it("treats a string ban flag in update-user as a ban", async () => {
    const { auth, self, headers } = await twoAdmins();
    await expect(
      auth.api.adminUpdateUser({ body: { userId: self, data: { banned: "true" } }, headers }),
    ).rejects.toThrow(mod.SELF_ADMIN_REMOVAL_ERROR);
  });

  it("does not count a role that merely contains \"admin\" as a remaining admin", async () => {
    const { store, auth, self, other, headers } = await twoAdmins();
    const { LAST_ADMIN_REMOVAL_ERROR } = mod;
    const ctx = await auth.$context;
    // The caller is banned (mirrors the "last active admin" test above, so
    // its own session still works but it no longer counts as a saving admin),
    // and a decoy user carries a role that would match a substring/`contains`
    // filter on "admin" without being exactly "admin".
    await ctx.internalAdapter.updateUser(self, { banned: true });
    await addUser(auth, "decoy@example.com", { role: "administrator", status: "active" });

    await expect(auth.api.setRole({ body: { userId: other, role: "user" }, headers })).rejects.toThrow(
      LAST_ADMIN_REMOVAL_ERROR,
    );
    expect(userRow(store, other)).toMatchObject({ role: "admin", status: "active" });
  });

  it("does not guard changes that keep admin access", async () => {
    const { store, auth, self, headers } = await twoAdmins();
    await auth.api.setRole({ body: { userId: self, role: "admin" }, headers });
    await auth.api.adminUpdateUser({ body: { userId: self, data: { name: "Renamed" } }, headers });
    expect(userRow(store, self)).toMatchObject({ role: "admin", name: "Renamed" });
  });
});
