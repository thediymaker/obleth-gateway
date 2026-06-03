import bcrypt from "bcryptjs";

export async function verifyCredentials(username: string, password: string) {
  const expectedUser = process.env.DASHBOARD_USERNAME;
  const passwordHash = process.env.DASHBOARD_PASSWORD_HASH;
  const plainPassword = process.env.DASHBOARD_PASSWORD;

  if (!expectedUser) {
    throw new Error("DASHBOARD_USERNAME is not set.");
  }
  if (!passwordHash && !plainPassword) {
    throw new Error(
      "No dashboard password configured. Set DASHBOARD_PASSWORD_HASH (bcrypt, recommended) or DASHBOARD_PASSWORD.",
    );
  }

  if (username !== expectedUser) return false;

  if (passwordHash) {
    return bcrypt.compare(password, passwordHash);
  }

  return password === plainPassword;
}
