-- Idempotent: applied on gateway boot and safe to re-run.
-- The control-plane "projects" feature creates tenants with
-- fairshare_group = 'projects' (tenants.fairshare_group references
-- fairshare_groups(name)), so the group must exist for project creation
-- to pass the tenants_fairshare_group_fkey constraint.
insert into fairshare_groups (name, weight)
values ('projects', 100)
on conflict (name) do nothing;
