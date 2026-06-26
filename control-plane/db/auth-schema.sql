-- better-auth identity tables for the obleth control plane. Idempotent: applied
-- at control-plane startup (instrumentation.ts) and safe to re-run.
--
-- Column names/types mirror better-auth 1.6.21's generated schema (incl. the
-- admin plugin's banned/banReason/banExpires + session.impersonatedBy). The
-- obleth custom columns are role, status and tenantId. tenantId references the
-- gateway-owned "tenants" table that already exists in the same database; the
-- control plane never writes that table (read-only FK).

create table if not exists "user" (
  id text primary key,
  name text not null,
  email text not null unique,
  "emailVerified" boolean not null default false,
  image text,
  "createdAt" timestamptz not null default now(),
  "updatedAt" timestamptz not null default now(),
  -- admin plugin fields
  "banned" boolean,
  "banReason" text,
  "banExpires" timestamptz,
  -- obleth custom fields
  role text not null default 'user',
  status text not null default 'pending',
  "tenantId" uuid references tenants(id) on delete set null
);
-- Custom/plugin columns added defensively for pre-existing "user" tables.
alter table "user" add column if not exists "banned" boolean;
alter table "user" add column if not exists "banReason" text;
alter table "user" add column if not exists "banExpires" timestamptz;
alter table "user" add column if not exists role text not null default 'user';
alter table "user" add column if not exists status text not null default 'pending';
alter table "user" add column if not exists "tenantId" uuid references tenants(id) on delete set null;

create table if not exists "session" (
  id text primary key,
  "expiresAt" timestamptz not null,
  token text not null unique,
  "createdAt" timestamptz not null default now(),
  "updatedAt" timestamptz not null default now(),
  "ipAddress" text,
  "userAgent" text,
  "userId" text not null references "user"(id) on delete cascade,
  -- admin plugin field
  "impersonatedBy" text
);
alter table "session" add column if not exists "impersonatedBy" text;
create index if not exists "session_userId_idx" on "session" ("userId");

create table if not exists "account" (
  id text primary key,
  "accountId" text not null,
  "providerId" text not null,
  "userId" text not null references "user"(id) on delete cascade,
  "accessToken" text,
  "refreshToken" text,
  "idToken" text,
  "accessTokenExpiresAt" timestamptz,
  "refreshTokenExpiresAt" timestamptz,
  scope text,
  password text,
  "createdAt" timestamptz not null default now(),
  "updatedAt" timestamptz not null default now()
);
create index if not exists "account_userId_idx" on "account" ("userId");

create table if not exists "verification" (
  id text primary key,
  identifier text not null,
  value text not null,
  "expiresAt" timestamptz not null,
  "createdAt" timestamptz not null default now(),
  "updatedAt" timestamptz not null default now()
);
create index if not exists "verification_identifier_idx" on "verification" ("identifier");
