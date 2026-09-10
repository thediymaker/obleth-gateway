-- Identity keys: an api_keys row that stands for a verified external identity
-- (issuer + subject from a JWT) rather than a minted secret. key_hash holds the
-- lookup handle `jwt:<sha256(issuer \0 subject)>`, so the data plane resolves
-- it exactly like a secret key. kind = 'secret' | 'identity'.
alter table api_keys add column if not exists kind text not null default 'secret';
alter table api_keys add column if not exists identity_issuer text;
alter table api_keys add column if not exists identity_subject text;
alter table api_keys add column if not exists identity_claims jsonb;

-- One identity key per (issuer, subject); the JIT upsert conflicts on this.
create unique index if not exists api_keys_identity_idx
    on api_keys (identity_issuer, identity_subject) where kind = 'identity';
