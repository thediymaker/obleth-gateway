use clap::{Parser, ValueEnum};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Target {
    /// Local, GPU-free demo backend (formerly "fixture"). `fixture` still works
    /// as an alias so existing scripts don't break.
    #[value(alias = "fixture")]
    Demo,
    Live,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Profile { Smoke, Light, Heavy, Extreme, Auto, Manual }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope { Single(String), All }

pub fn scope_from(model: Option<String>, all: bool) -> Scope {
    match model {
        Some(m) if !all => Scope::Single(m),
        _ => Scope::All,
    }
}

#[derive(Parser, Debug, Clone)]
#[command(name = "obench", version, about = "obleth benchmark & readiness suite")]
pub struct Cli {
    #[arg(long)]
    pub target: Option<Target>,
    #[arg(long)]
    pub profile: Option<Profile>,
    /// Drive a single named model (Scope::Single). Ignored if --all is set.
    #[arg(long)]
    pub model: Option<String>,
    /// Drive the whole fleet (Scope::All). Default when neither is given.
    #[arg(long)]
    pub all: bool,

    #[arg(long, env = "ADMIN_BASE", default_value = "http://localhost:9180")]
    pub admin_base: String,
    #[arg(long, env = "ADMIN_TOKEN", default_value = "dev-admin-token")]
    pub admin_token: String,
    #[arg(long, env = "PROXY_BASE", default_value = "http://localhost:8088")]
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
    #[arg(long, default_value = "live.config.json")]
    pub config: String,
    /// Force headless even with no subcommand.
    #[arg(long)]
    pub no_tui: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_without_all_is_single_scope() {
        assert_eq!(scope_from(Some("m1".into()), false), Scope::Single("m1".into()));
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
            "obench", "--target", "fixture", "--profile", "heavy", "--all",
        ]).unwrap();
        assert_eq!(cli.target, Some(Target::Demo));
        assert_eq!(cli.profile, Some(Profile::Heavy));
        assert!(cli.all);
    }
}
