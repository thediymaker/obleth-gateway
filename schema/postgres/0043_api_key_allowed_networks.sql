-- Idempotent: safe to re-run.
-- Networks a key may be used from, as canonical CIDR text (a single address
-- is a /32 or /128). Empty means any network. The admin API validates and
-- normalises entries before they land here.
alter table api_keys add column if not exists allowed_networks text[] not null default '{}';
