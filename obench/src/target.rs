use crate::cli::{Profile, Target};

/// Reject combinations that cannot produce a meaningful result.
pub fn validate_combo(target: Target, profile: Profile) -> Result<(), String> {
    if target == Target::Live && profile == Profile::Extreme {
        return Err(
            "extreme measures the gateway's max req/s and needs the GPU-free fixture; \
             against live upstreams the generation time dominates. Use --target fixture, \
             or pick auto/heavy for live."
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_extreme_is_rejected_with_reason() {
        let err = validate_combo(Target::Live, Profile::Extreme).unwrap_err();
        assert!(err.contains("fixture"));
    }

    #[test]
    fn fixture_extreme_is_allowed() {
        assert!(validate_combo(Target::Fixture, Profile::Extreme).is_ok());
    }

    #[test]
    fn live_heavy_is_allowed() {
        assert!(validate_combo(Target::Live, Profile::Heavy).is_ok());
    }
}
