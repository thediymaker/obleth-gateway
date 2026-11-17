-- Idempotent: safe to re-run.
-- Countries a key may be used from, as upper-case ISO 3166-1 alpha-2 codes.
-- A request is allowed when its client address is in allowed_networks or its
-- client country (set by the edge, public addresses only) is listed here.
-- Both empty means any network. The admin API validates and normalises
-- entries before they land here.
alter table api_keys add column if not exists allowed_countries text[] not null default '{}';
