use clap::{Parser, Subcommand, ValueEnum};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Target {
    /// Local, GPU-free demo backend (formerly "fixture"). `fixture` still works
    /// as an alias so existing scripts don't break.
    #[value(alias = "fixture")]
    Demo,
    Live,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Profile {
    Smoke,
    Light,
    Heavy,
    Extreme,
    Auto,
    Manual,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    Single(String),
    All,
}

pub fn scope_from(model: Option<String>, all: bool) -> Scope {
    match model {
        Some(m) if !all => Scope::Single(m),
        _ => Scope::All,
    }
}

#[derive(Parser, Debug, Clone)]
#[command(name = "obench", version, about = "obleth benchmark & readiness suite")]
pub struct Cli {
    #[arg(long, global = true)]
    pub target: Option<Target>,
    #[arg(long)]
    pub profile: Option<Profile>,
    /// Drive a single named model (Scope::Single). Ignored if --all is set.
    #[arg(long, global = true)]
    pub model: Option<String>,
    /// Drive the whole fleet (Scope::All). Default when neither is given.
    #[arg(long, global = true)]
    pub all: bool,

    #[arg(
        long,
        env = "ADMIN_BASE",
        default_value = "http://localhost:9180",
        global = true
    )]
    pub admin_base: String,
    #[arg(
        long,
        env = "ADMIN_TOKEN",
        default_value = "dev-admin-token",
        global = true
    )]
    pub admin_token: String,
    #[arg(
        long,
        env = "PROXY_BASE",
        default_value = "http://localhost:8088",
        global = true
    )]
    pub proxy_base: String,
    #[arg(long, env = "UI_BASE", default_value = "http://localhost:3002")]
    pub ui_base: String,

    #[arg(long)]
    pub conc: Option<u32>,
    #[arg(long)]
    pub output_tokens: Option<u32>,
    #[arg(long, default_value_t = 256)]
    pub input_tokens: u32,
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub stream: bool,
    #[arg(long)]
    pub duration_s: Option<u64>,
    #[arg(long)]
    pub capacity: Option<u32>,
    #[arg(long)]
    pub max_error_rate: Option<f64>,
    /// Path to live config for headless `--target live` (remote obleth proxy
    /// URL + tenant keys + models). The interactive TUI builds this for you.
    #[arg(long, default_value = "live.config.json", global = true)]
    pub config: String,
    /// Force headless even with no subcommand.
    #[arg(long)]
    pub no_tui: bool,

    /// Optional benchmark subcommand. Absent → the load/readiness benchmark
    /// (the historical default; all the flags above apply to it).
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Compression boon A/B — measure token savings + latency crossover.
    Compression(CompressionArgs),
    /// Deployment scorecard — benchmark every measurable aspect and grade it.
    Score(ScoreArgs),
}

#[derive(clap::Args, Debug, Clone)]
pub struct CompressionArgs {
    /// Model to call; must have the `compression` boon granted.
    #[arg(long, env = "MODEL")]
    pub model: String,
    /// API key the requests authenticate with.
    #[arg(long, env = "OBLETH_API_KEY")]
    pub api_key: String,
    /// Input price $/1M tokens for the cost estimate.
    #[arg(long, env = "PRICE_IN_PER_MTOK", default_value_t = crate::benchmarks::compression::DEFAULT_PRICE_IN_PER_MTOK)]
    pub price_per_mtok: f64,
    /// Comma-separated prefill tokens/sec to model for the crossover.
    // keep in sync with compression::default_prefill_tps()
    #[arg(
        long,
        env = "PREFILL_TPS",
        value_delimiter = ',',
        default_value = "500,2000,8000"
    )]
    pub prefill_tps: Vec<u32>,
    /// Timed repetitions per arm (median reported).
    #[arg(long, env = "REPS", default_value_t = crate::benchmarks::compression::DEFAULT_REPS)]
    pub reps: u32,
    /// Output tokens to request (keep small + constant so fixture latency is fixed).
    #[arg(long, env = "MAX_TOKENS", default_value_t = crate::benchmarks::compression::DEFAULT_MAX_TOKENS)]
    pub max_tokens: u32,
    /// Per-segment min_tokens floor to set during the run (restored after).
    #[arg(long, env = "BENCH_MIN_TOKENS", default_value_t = crate::benchmarks::compression::DEFAULT_MIN_TOKENS)]
    pub min_tokens: u32,
}

impl CompressionArgs {
    /// Build the run config, taking connection settings from the top-level Cli.
    pub fn into_config(self, cli: &Cli) -> crate::benchmarks::compression::CompressionConfig {
        crate::benchmarks::compression::CompressionConfig {
            proxy_base: cli.proxy_base.clone(),
            admin_base: cli.admin_base.clone(),
            admin_token: cli.admin_token.clone(),
            api_key: self.api_key,
            model: self.model,
            price_in_per_mtok: self.price_per_mtok,
            prefill_tps: self.prefill_tps,
            max_tokens: self.max_tokens,
            reps: self.reps,
            min_tokens: self.min_tokens,
        }
    }
}

#[derive(clap::Args, Debug, Clone)]
pub struct ScoreArgs {
    /// Skip the per-model capacity ramps (capacity/overload report as skipped).
    #[arg(long)]
    pub quick: bool,
    /// Sections to skip (csv): overhead,capacity,overload,streaming,resilience,fairshare,compression
    #[arg(long, value_delimiter = ',')]
    pub skip: Vec<String>,
    /// Run only these sections (csv). Empty = all applicable.
    #[arg(long, value_delimiter = ',')]
    pub only: Vec<String>,
    /// Concurrency cap for capacity ramps (live safety valve).
    #[arg(long, default_value_t = 256)]
    pub max_conc: u32,
    /// Direct URL of benchmark-backend as reachable from THIS machine (demo overhead check).
    #[arg(long, env = "BACKEND_BASE", default_value = "http://localhost:8081")]
    pub backend_base: String,
    /// Explicit baseline scorecard JSON to diff against (default: latest for target).
    #[arg(long)]
    pub baseline: Option<String>,
    /// Exit nonzero if the system score is below this (also on flagged regressions).
    #[arg(long)]
    pub fail_under: Option<u8>,
    /// Model with the compression boon granted — enables the compression section.
    #[arg(long)]
    pub compression_model: Option<String>,
    /// API key for the compression model.
    #[arg(long, env = "OBLETH_API_KEY")]
    pub compression_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_without_all_is_single_scope() {
        assert_eq!(
            scope_from(Some("m1".into()), false),
            Scope::Single("m1".into())
        );
    }

    #[test]
    fn all_flag_wins_over_model() {
        assert_eq!(scope_from(Some("m1".into()), true), Scope::All);
    }

    #[test]
    fn no_model_defaults_to_all() {
        assert_eq!(scope_from(None, false), Scope::All);
    }

    #[test]
    fn parses_headless_flags() {
        let cli = Cli::try_parse_from([
            "obench",
            "--target",
            "fixture",
            "--profile",
            "heavy",
            "--all",
        ])
        .unwrap();
        assert_eq!(cli.target, Some(Target::Demo));
        assert_eq!(cli.profile, Some(Profile::Heavy));
        assert!(cli.all);
    }

    #[test]
    fn bare_load_invocation_still_parses() {
        let cli =
            Cli::try_parse_from(["obench", "--target", "demo", "--profile", "smoke", "--all"])
                .unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.target, Some(Target::Demo));
    }

    #[test]
    fn compression_subcommand_parses() {
        let cli = Cli::try_parse_from([
            "obench",
            "compression",
            "--model",
            "m1",
            "--api-key",
            "sk-x",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Compression(a)) => {
                assert_eq!(a.model, "m1");
                assert_eq!(a.api_key, "sk-x");
                assert_eq!(a.reps, 5);
                assert_eq!(a.prefill_tps, vec![500, 2000, 8000]);
            }
            _ => panic!("expected compression subcommand"),
        }
    }

    #[test]
    fn compression_prefill_tps_parses_csv() {
        let cli = Cli::try_parse_from([
            "obench",
            "compression",
            "--model",
            "m",
            "--api-key",
            "k",
            "--prefill-tps",
            "100,200",
        ])
        .unwrap();
        let Some(Command::Compression(a)) = cli.command else {
            panic!()
        };
        assert_eq!(a.prefill_tps, vec![100, 200]);
    }

    #[test]
    fn score_subcommand_parses_with_global_target() {
        let cli = Cli::try_parse_from(["obench", "score", "--target", "demo", "--quick"]).unwrap();
        assert_eq!(cli.target, Some(Target::Demo));
        match cli.command {
            Some(Command::Score(a)) => {
                assert!(a.quick);
                assert_eq!(a.max_conc, 256);
                assert_eq!(a.backend_base, "http://localhost:8081");
                assert!(a.skip.is_empty());
                assert!(a.fail_under.is_none());
            }
            _ => panic!("expected score subcommand"),
        }
    }

    #[test]
    fn score_skip_parses_csv() {
        let cli = Cli::try_parse_from([
            "obench",
            "score",
            "--target",
            "demo",
            "--skip",
            "fairshare,resilience",
        ])
        .unwrap();
        let Some(Command::Score(a)) = cli.command else {
            panic!()
        };
        assert_eq!(
            a.skip,
            vec!["fairshare".to_string(), "resilience".to_string()]
        );
    }
}
