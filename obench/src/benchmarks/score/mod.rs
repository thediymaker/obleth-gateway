#![allow(dead_code)] // consumed as score sections land (tasks 5-13)

//! Deployment scorecard: orchestrates the check sections, rolls their scores
//! into one graded system score, and tracks regressions across runs.

pub mod capacity;
// pub mod fairshare;   // (files created in later tasks — add these mod lines as the files land)
// pub mod overhead;
pub mod overload;
// pub mod report;
// pub mod resilience;
pub mod streaming;

use serde::Serialize;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionId {
    Overhead,
    Capacity,
    Overload,
    Streaming,
    Resilience,
    Fairshare,
    Compression,
}

impl SectionId {
    pub fn name(self) -> &'static str {
        match self {
            SectionId::Overhead => "overhead",
            SectionId::Capacity => "capacity",
            SectionId::Overload => "overload",
            SectionId::Streaming => "streaming",
            SectionId::Resilience => "resilience",
            SectionId::Fairshare => "fairshare",
            SectionId::Compression => "compression",
        }
    }

    pub fn from_name(s: &str) -> Option<SectionId> {
        match s {
            "overhead" => Some(SectionId::Overhead),
            "capacity" => Some(SectionId::Capacity),
            "overload" => Some(SectionId::Overload),
            "streaming" => Some(SectionId::Streaming),
            "resilience" => Some(SectionId::Resilience),
            "fairshare" => Some(SectionId::Fairshare),
            "compression" => Some(SectionId::Compression),
            _ => None,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Grade {
    A,
    B,
    C,
    D,
    F,
    Skipped,
    Errored,
}

impl Grade {
    pub fn letter(self) -> &'static str {
        match self {
            Grade::A => "A",
            Grade::B => "B",
            Grade::C => "C",
            Grade::D => "D",
            Grade::F => "F",
            Grade::Skipped => "—",
            Grade::Errored => "ERR",
        }
    }
}

pub fn grade_from_score(score: u8) -> Grade {
    match score {
        90..=u8::MAX => Grade::A,
        75..=89 => Grade::B,
        60..=74 => Grade::C,
        45..=59 => Grade::D,
        _ => Grade::F,
    }
}

#[derive(Debug, Serialize)]
pub struct SectionResult {
    pub id: SectionId,
    pub score: Option<u8>,
    pub metrics: serde_json::Value,
    pub recommendations: Vec<String>,
    pub error: Option<String>,
}

impl SectionResult {
    pub fn grade(&self) -> Grade {
        if self.error.is_some() {
            return Grade::Errored;
        }
        match self.score {
            Some(s) => grade_from_score(s),
            None => Grade::Skipped,
        }
    }

    pub fn skipped(id: SectionId, why: &str) -> SectionResult {
        SectionResult {
            id,
            score: None,
            metrics: serde_json::json!({ "skipped": why }),
            recommendations: vec![],
            error: None,
        }
    }

    pub fn errored(id: SectionId, err: &str) -> SectionResult {
        SectionResult {
            id,
            score: None,
            metrics: serde_json::json!({}),
            recommendations: vec![],
            error: Some(err.to_string()),
        }
    }
}

/// Section weights per target. Sections missing from a target's list are not
/// run there at all; sections that run but end Skipped/Errored redistribute
/// their weight via `system_score`.
pub fn weights(target: crate::cli::Target) -> Vec<(SectionId, u32)> {
    use crate::cli::Target::*;
    match target {
        Demo => vec![
            (SectionId::Overhead, 15),
            (SectionId::Capacity, 20),
            (SectionId::Overload, 15),
            (SectionId::Streaming, 10),
            (SectionId::Resilience, 20),
            (SectionId::Fairshare, 15),
            (SectionId::Compression, 5),
        ],
        Live => vec![
            (SectionId::Capacity, 45),
            (SectionId::Overload, 25),
            (SectionId::Streaming, 20),
            (SectionId::Compression, 10),
        ],
    }
}

/// Weighted mean over sections that produced a score; weights of unscored
/// sections are redistributed by normalizing over the scored subset.
pub fn system_score(results: &[SectionResult], weights: &[(SectionId, u32)]) -> Option<u8> {
    let mut num = 0f64;
    let mut den = 0f64;
    for (id, w) in weights {
        if let Some(r) = results.iter().find(|r| r.id == *id) {
            if let Some(s) = r.score {
                num += s as f64 * *w as f64;
                den += *w as f64;
            }
        }
    }
    if den == 0.0 {
        None
    } else {
        Some((num / den).round() as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Target;

    fn scored(id: SectionId, score: u8) -> SectionResult {
        SectionResult {
            id,
            score: Some(score),
            metrics: serde_json::json!({}),
            recommendations: vec![],
            error: None,
        }
    }

    #[test]
    fn grade_thresholds() {
        assert_eq!(grade_from_score(90), Grade::A);
        assert_eq!(grade_from_score(75), Grade::B);
        assert_eq!(grade_from_score(60), Grade::C);
        assert_eq!(grade_from_score(45), Grade::D);
        assert_eq!(grade_from_score(44), Grade::F);
    }

    #[test]
    fn weights_sum_to_100_for_both_targets() {
        for t in [Target::Demo, Target::Live] {
            let sum: u32 = weights(t).iter().map(|(_, w)| w).sum();
            assert_eq!(sum, 100, "{t:?}");
        }
    }

    #[test]
    fn live_has_no_demo_only_sections() {
        let ids: Vec<SectionId> = weights(Target::Live).iter().map(|(i, _)| *i).collect();
        assert!(!ids.contains(&SectionId::Overhead));
        assert!(!ids.contains(&SectionId::Resilience));
        assert!(!ids.contains(&SectionId::Fairshare));
    }

    #[test]
    fn system_score_is_weighted_mean() {
        let w = vec![(SectionId::Capacity, 60), (SectionId::Streaming, 40)];
        let r = vec![
            scored(SectionId::Capacity, 100),
            scored(SectionId::Streaming, 50),
        ];
        assert_eq!(system_score(&r, &w), Some(80)); // 100*0.6 + 50*0.4
    }

    #[test]
    fn skipped_sections_redistribute_weight() {
        let w = vec![(SectionId::Capacity, 60), (SectionId::Streaming, 40)];
        let r = vec![
            scored(SectionId::Capacity, 80),
            SectionResult::skipped(SectionId::Streaming, "quick mode"),
        ];
        assert_eq!(system_score(&r, &w), Some(80)); // streaming weight redistributed
    }

    #[test]
    fn all_skipped_gives_none() {
        let w = vec![(SectionId::Capacity, 100)];
        let r = vec![SectionResult::skipped(SectionId::Capacity, "x")];
        assert_eq!(system_score(&r, &w), None);
    }

    #[test]
    fn errored_grade_and_skipped_grade() {
        assert_eq!(
            SectionResult::errored(SectionId::Overhead, "boom").grade(),
            Grade::Errored
        );
        assert_eq!(
            SectionResult::skipped(SectionId::Overhead, "n/a").grade(),
            Grade::Skipped
        );
        assert_eq!(scored(SectionId::Overhead, 91).grade(), Grade::A);
    }

    #[test]
    fn section_id_names_roundtrip() {
        for id in [
            SectionId::Overhead,
            SectionId::Capacity,
            SectionId::Overload,
            SectionId::Streaming,
            SectionId::Resilience,
            SectionId::Fairshare,
            SectionId::Compression,
        ] {
            assert_eq!(SectionId::from_name(id.name()), Some(id));
        }
        assert_eq!(SectionId::from_name("bogus"), None);
    }
}
