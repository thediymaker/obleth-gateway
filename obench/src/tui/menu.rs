use crate::cli::{Profile, Target};
use crate::target::validate_combo;

pub const ALL_PROFILES: &[Profile] = &[
    Profile::Smoke,
    Profile::Light,
    Profile::Heavy,
    Profile::Extreme,
    Profile::Auto,
    Profile::Manual,
];

/// Return each profile with whether it is valid for the chosen target.
pub fn valid_profiles(target: Target) -> Vec<(Profile, bool)> {
    ALL_PROFILES
        .iter()
        .map(|p| (*p, validate_combo(target, *p).is_ok()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extreme_disabled_for_live() {
        let v = valid_profiles(Target::Live);
        let extreme = v.iter().find(|(p, _)| *p == Profile::Extreme).unwrap();
        assert!(!extreme.1);
    }

    #[test]
    fn all_enabled_for_fixture() {
        assert!(valid_profiles(Target::Fixture).iter().all(|(_, ok)| *ok));
    }
}
