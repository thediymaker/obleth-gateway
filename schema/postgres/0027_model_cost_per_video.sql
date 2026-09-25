-- Idempotent: safe to re-run.
-- Flat price of one video generation job (`video` models), in USD. Charged
-- once, when the create call (`POST /v1/videos`) succeeds; the polls and the
-- download that follow are not billed. Zero, like the other per-unit prices,
-- until an operator sets it.
alter table models add column if not exists cost_per_video double precision not null default 0;
