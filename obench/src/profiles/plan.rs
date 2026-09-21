use crate::cli::{Cli, Profile};

#[derive(Clone, Copy, Debug)]
pub struct ProfilePlan {
    pub conc: u32,
    pub duration_s: u64,
    pub warmup_s: u64,
    /// Per-model admission pool size. The gateway's global ceiling is set to
    /// this times the number of seeded models, so the pools — not the ceiling —
    /// are what gates admission.
    pub capacity: u32,
    pub output_tokens: u32,
    pub max_error_rate: f64,
    pub stream: bool,
}

pub fn resolve(profile: Profile, cli: &Cli) -> ProfilePlan {
    let base = base_plan(profile);
    ProfilePlan {
        conc: cli.conc.unwrap_or(base.conc),
        duration_s: cli.duration_s.unwrap_or(base.duration_s),
        warmup_s: base.warmup_s,
        capacity: cli.capacity.unwrap_or(base.capacity),
        output_tokens: cli.output_tokens.unwrap_or(base.output_tokens),
        max_error_rate: cli.max_error_rate.unwrap_or(base.max_error_rate),
        stream: cli.stream,
    }
}

/// The per-profile baseline before any CLI/TUI overrides. Exposed so the wizard
/// can seed its editable concurrency / output-token knobs from the profile.
pub fn base_plan(profile: Profile) -> ProfilePlan {
    // Per-profile defaults (carried from the .mjs harness as a starting point).
    match profile {
        // smoke: bounded 30-second, 2-worker CI ping. conc=2 keeps load minimal
        // while still exercising the weighted picker across all fixture models;
        // 30 s is enough to cycle through all 5 models statistically.
        // capacity is per-model slots, and 64 of them against 2 workers means
        // admission never gates — deliberate: smoke checks liveness, not queueing.
        // duration_s > 0 is required: 0 means "run until quit" and would hang headless.
        Profile::Smoke => ProfilePlan {
            conc: 2,
            duration_s: 30,
            warmup_s: 0,
            capacity: 64,
            output_tokens: 16,
            max_error_rate: 0.0,
            stream: true,
        },
        // light: 2 slots per model across the 5 fixture models is 10 against 16
        // workers, so the scheduler arbitrates rather than waving everything
        // through. (capacity is per-model now, so the old global 64 would have
        // become 320 and gated nothing.)
        Profile::Light => ProfilePlan {
            conc: 16,
            duration_s: 60,
            warmup_s: 3,
            capacity: 2,
            output_tokens: 64,
            max_error_rate: 0.05,
            stream: true,
        },
        // heavy: 8 slots per model, 40 across the fleet against 64 workers —
        // sustained contention for the long soak.
        Profile::Heavy => ProfilePlan {
            conc: 64,
            duration_s: 600,
            warmup_s: 5,
            capacity: 8,
            output_tokens: 128,
            max_error_rate: 0.05,
            stream: true,
        },
        // extreme: max-push throughput ceiling. capacity == conc is the exact
        // "never gate on admission" value (closed loop holds <= conc in flight),
        // so the gateway's concurrency limit is never the bottleneck and we
        // measure raw req/s. 2048 mirrors the old max.mjs default fan-out.
        Profile::Extreme => ProfilePlan {
            conc: 2048,
            duration_s: 30,
            warmup_s: 3,
            capacity: 2048,
            output_tokens: 4,
            max_error_rate: 0.01,
            stream: false,
        },
        // fairshare: many pools, many keys, every pool saturated. conc is set
        // in build_setup to 2 x key count, and FAIRSHARE_TRAFFIC drives all
        // eight seeded models, so 8 slots per model leaves every pool contended
        // and shares reflect admission rather than offered load.
        Profile::Fairshare => ProfilePlan {
            conc: 0,
            duration_s: 90,
            warmup_s: 5,
            capacity: 8,
            output_tokens: 48,
            max_error_rate: 0.02,
            stream: true,
        },
        Profile::Auto => ProfilePlan {
            conc: 32,
            duration_s: 15,
            warmup_s: 2,
            capacity: 100000,
            output_tokens: 4,
            max_error_rate: 0.01,
            stream: false,
        },
        // manual: the hand-tuned preset, so it starts where heavy does — 8
        // per-model slots against 64 workers — and every knob is overridable.
        Profile::Manual => ProfilePlan {
            conc: 64,
            duration_s: 60,
            warmup_s: 3,
            capacity: 8,
            output_tokens: 64,
            max_error_rate: 0.05,
            stream: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn smoke_terminates_headless() {
        // duration_s == 0 means "run until quit" in run_closed_loop, which hangs
        // headless. Smoke MUST have a positive duration so the CI ping exits.
        let cli =
            Cli::try_parse_from(["obench", "--target", "fixture", "--profile", "smoke"]).unwrap();
        let plan = resolve(Profile::Smoke, &cli);
        assert!(
            plan.duration_s > 0,
            "smoke duration_s must be > 0 to terminate headless"
        );
    }

    #[test]
    fn extreme_defaults_to_buffered_tiny_output() {
        let cli =
            Cli::try_parse_from(["obench", "--target", "fixture", "--profile", "extreme"]).unwrap();
        let plan = resolve(Profile::Extreme, &cli);
        assert_eq!(plan.output_tokens, 4);
        assert_eq!(plan.capacity, 2048);
    }

    #[test]
    fn cli_flag_overrides_default() {
        let cli = Cli::try_parse_from([
            "obench",
            "--target",
            "fixture",
            "--profile",
            "heavy",
            "--conc",
            "200",
        ])
        .unwrap();
        assert_eq!(resolve(Profile::Heavy, &cli).conc, 200);
    }
}
