-- Idempotent: safe to re-run.
-- v2.7.0 started every new key with `obl_`; from v2.8.0 the built-in prefix
-- is `sk` and Settings › Keys can choose another. An instance that already
-- made `obl_` keys keeps making them: save `obl` as its setting unless one
-- is saved already. A later change in Settings is never overwritten.
insert into app_settings (key, value, updated_at)
select 'key_prefix', '{"prefix": "obl"}'::jsonb, now()
where exists (select 1 from api_keys where key_prefix like 'obl\_%')
on conflict (key) do nothing;
