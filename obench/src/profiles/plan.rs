use crate::cli::{Cli, Profile};

#[derive(Clone, Copy, Debug)]
pub struct ProfilePlan {
    pub conc: u32,
    pub duration_s: u64,
    pub warmup_s: u64,
    pub capacity: u32,
    pub output_tokens: u32,
    pub max_error_rate: f64,
    pub stream: bool,
}

pub fn resolve(profile: Profile, cli: &Cli) -> ProfilePlan {
    // Per-profile defaults (carried from the .mjs harness as a starting point).
    let base = match profile {
        // smoke: bounded 30-second, 2-worker CI ping. conc=2 keeps load minimal
        // while still exercising the weighted picker across all fixture models;
        // 30 s is enough to cycle through all 5 models statistically.
        // duration_s > 0 is required: 0 means "run until quit" and would hang headless.
        Profile::Smoke   => ProfilePlan { conc: 2,    duration_s: 30,  warmup_s: 0, capacity: 64,    output_tokens: 16,  max_error_rate: 0.0,  stream: true },
        Profile::Light   => ProfilePlan { conc: 16,   duration_s: 60,  warmup_s: 3, capacity: 64,    output_tokens: 64,  max_error_rate: 0.05, stream: true },
        Profile::Heavy   => ProfilePlan { conc: 64,   duration_s: 600, warmup_s: 5, capacity: 64,    output_tokens: 128, max_error_rate: 0.05, stream: true },
        Profile::Extreme => ProfilePlan { conc: 256,  duration_s: 30,  warmup_s: 3, capacity: 256,   output_tokens: 4,   max_error_rate: 0.01, stream: false },
        Profile::Auto    => ProfilePlan { conc: 32,   duration_s: 15,  warmup_s: 2, capacity: 100000,output_tokens: 4,   max_error_rate: 0.01, stream: false },
        Profile::Manual  => ProfilePlan { conc: 64,   duration_s: 60,  warmup_s: 3, capacity: 64,    output_tokens: 64,  max_error_rate: 0.05, stream: true },
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn smoke_terminates_headless() {
        // duration_s == 0 means "run until quit" in run_closed_loop, which hangs
        // headless. Smoke MUST have a positive duration so the CI ping exits.
        let cli = Cli::try_parse_from(["obench", "--target", "fixture", "--profile", "smoke"]).unwrap();
        let plan = resolve(Profile::Smoke, &cli);
        assert!(plan.duration_s > 0, "smoke duration_s must be > 0 to terminate headless");
    }

    #[test]
    fn extreme_defaults_to_buffered_tiny_output() {
        let cli = Cli::try_parse_from(["obench", "--target", "fixture", "--profile", "extreme"]).unwrap();
        let plan = resolve(Profile::Extreme, &cli);
        assert_eq!(plan.output_tokens, 4);
        assert_eq!(plan.capacity, 256);
    }

    #[test]
    fn cli_flag_overrides_default() {
        let cli = Cli::try_parse_from(["obench", "--target", "fixture", "--profile", "heavy", "--conc", "200"]).unwrap();
        assert_eq!(resolve(Profile::Heavy, &cli).conc, 200);
    }
}
