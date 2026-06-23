//! Library surface of the obleth-provisioner crate.
//!
//! Exposes the domain types and slurmrestd client so other crates (e.g.
//! obleth-admin) can call `discover_resources()` without depending on the
//! binary's entry point.

pub mod domain;
pub mod slurm;
