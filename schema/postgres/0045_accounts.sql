alter table tenants add column if not exists kind text not null default 'standard';
do $$ begin
  if not exists (select 1 from pg_constraint where conname = 'tenants_kind_check') then
    alter table tenants add constraint tenants_kind_check check (kind in ('standard','personal'));
  end if;
end $$;
alter table api_keys add column if not exists owner_issuer text;
alter table api_keys add column if not exists owner_subject text;
create index if not exists api_keys_owner_idx on api_keys (tenant_id, owner_issuer, owner_subject) where owner_subject is not null;
create table if not exists tenant_members (
  tenant_id uuid not null references tenants (id) on delete cascade,
  issuer text not null, subject text not null, email text,
  role text not null check (role in ('owner','manager','member')),
  created_at timestamptz not null default now(),
  primary key (tenant_id, issuer, subject)
);
create index if not exists tenant_members_identity_idx on tenant_members (issuer, subject);
