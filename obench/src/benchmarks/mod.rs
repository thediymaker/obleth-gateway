pub mod compression;
pub mod score;

/// Which benchmark obench is running. Load is the historical default; new kinds
/// (e.g. compressor capacity) add a variant + a module. Deliberately an enum,
/// not a trait — YAGNI until a third kind needs a shared interface.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BenchKind {
    Load,
    Compression,
    #[allow(dead_code)] // consumed in task 13
    Score,
}
