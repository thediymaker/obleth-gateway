-- Replica rows must outlive their model row so the provisioner's drain pass can
-- cancel the Slurm jobs they represent before the rows disappear. The original
-- ON DELETE CASCADE (0001) destroyed them in the same statement as the model,
-- stranding running jobs. model_id stays NOT NULL; only referential enforcement
-- is dropped -- the replica row's lifecycle is owned by the provisioner.
alter table model_replicas drop constraint if exists model_replicas_model_id_fkey;
