//! Code pruning groundwork (Tree-sitter, planned).
//!
//! Code is dangerous to compress with token-droppers — dropping a `}` or `return`
//! breaks the syntax tree the upstream model reads. The plan is to parse code
//! into an AST with Tree-sitter and prune *structurally*: strip comments and
//! docstrings, then (later) collapse function bodies not relevant to the active
//! query — always emitting syntactically valid code.
//!
//! This module ships the integration point and language detection now; the
//! Tree-sitter grammars and the pruning itself land later. [`prune_code`] is a
//! fail-open no-op until then, so wiring it into the pipeline changes nothing.

// Scaffolding: these items are exercised by tests but not yet wired into the
// compression pipeline. Remove once `prune_code` is called from `apply`.
#![allow(dead_code)]

use std::collections::HashSet;

/// Languages we intend to support first (those with mature Tree-sitter grammars
/// and the highest payoff from comment/body pruning).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,
    Unknown,
}

/// Detect the language of a fenced code block. Prefers the fence info string
/// (```` ```rust ````), falling back to light content heuristics.
pub(crate) fn detect_language(fence_info: Option<&str>, body: &str) -> Lang {
    if let Some(info) = fence_info {
        match info.trim().to_ascii_lowercase().as_str() {
            "rust" | "rs" => return Lang::Rust,
            "python" | "py" => return Lang::Python,
            "javascript" | "js" | "jsx" => return Lang::JavaScript,
            "typescript" | "ts" | "tsx" => return Lang::TypeScript,
            "go" | "golang" => return Lang::Go,
            "" => {}
            _ => return Lang::Unknown,
        }
    }
    // Minimal content heuristics for unlabeled blocks.
    if body.contains("fn ") && body.contains("->") && body.contains("let ") {
        Lang::Rust
    } else if body.contains("def ") || body.contains("import ") && body.contains(":\n") {
        Lang::Python
    } else if body.contains("func ") && body.contains("package ") {
        Lang::Go
    } else if body.contains("function ") || body.contains("const ") || body.contains("=>") {
        Lang::JavaScript
    } else {
        Lang::Unknown
    }
}

/// Prune a code segment structurally. **Stub:** returns `None` (no change) until
/// the Tree-sitter grammars are wired. Planned behavior: parse `text` as `lang`,
/// strip comments/docstrings, collapse query-irrelevant function bodies to a
/// signature + `{ … }`, and return `Some(pruned)` only when it stays valid and is
/// strictly shorter. Fail-open by construction.
pub(crate) fn prune_code(
    _text: &str,
    _lang: Lang,
    _query_terms: &HashSet<String>,
) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_from_fence_info() {
        assert_eq!(detect_language(Some("rust"), ""), Lang::Rust);
        assert_eq!(detect_language(Some("py"), ""), Lang::Python);
        assert_eq!(detect_language(Some("tsx"), ""), Lang::TypeScript);
        assert_eq!(detect_language(Some("cobol"), ""), Lang::Unknown);
    }

    #[test]
    fn falls_back_to_content_heuristics() {
        assert_eq!(
            detect_language(None, "fn main() -> () { let x = 1; }"),
            Lang::Rust
        );
        assert_eq!(detect_language(None, "func main() {}\npackage x"), Lang::Go);
    }

    #[test]
    fn prune_is_noop_stub() {
        assert_eq!(prune_code("fn x() {}", Lang::Rust, &HashSet::new()), None);
    }
}
