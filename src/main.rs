use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};

use tokentab::adapters::claude_code::ClaudeCodeTranscripts;
use tokentab::adapters::pricing_toml::{self, Loaded};
use tokentab::adapters::render_json::JsonRenderer;
use tokentab::adapters::render_text::TextRenderer;
use tokentab::domain::money::Money;
use tokentab::domain::plans::{Billing, CreditsMode, EstimateOptions};
use tokentab::domain::report;
use tokentab::domain::usage::ScanFilter;
use tokentab::ports::renderer::Renderer;
use tokentab::ports::usage_source::UsageSource;

#[derive(Parser)]
#[command(name = "tokentab", version, about = "Replays local AI coding transcripts through a price list and compares what they would have cost on each plan")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan transcripts, price every request, and compare plans.
    Estimate(EstimateArgs),
    /// Print the models and plans in the price list.
    Prices(PricesArgs),
}

#[derive(clap::Args)]
struct EstimateArgs {
    /// Directory holding Claude Code transcripts (default: ~/.claude/projects).
    #[arg(long)]
    projects_dir: Option<PathBuf>,
    /// Only requests whose project path contains this substring.
    #[arg(long)]
    project: Option<String>,
    /// Only requests on or after this UTC date (YYYY-MM-DD).
    #[arg(long)]
    since: Option<String>,
    /// Only requests before this UTC date (YYYY-MM-DD).
    #[arg(long)]
    until: Option<String>,
    /// Price list TOML (default: the list compiled into the binary).
    #[arg(long)]
    pricing: Option<PathBuf>,
    /// Seats to bill for Team and Enterprise; plan minimums are applied and noted.
    #[arg(long, default_value_t = 1)]
    seats: u32,
    #[arg(long, value_enum, default_value_t = BillingArg::Monthly)]
    billing: BillingArg,
    /// Whether bundled Max/Team API credits offset SDK and headless usage.
    #[arg(long, value_enum, default_value_t = CreditsArg::Apply)]
    credits: CreditsArg,
    /// API-equivalent dollars one Pro seat gets per month; every allowance is a multiple of it.
    #[arg(long)]
    allowance_anchor: Option<f64>,
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
}

#[derive(clap::Args)]
struct PricesArgs {
    #[arg(long)]
    pricing: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum BillingArg {
    Monthly,
    Annual,
}

#[derive(Clone, Copy, ValueEnum)]
enum CreditsArg {
    Apply,
    Ignore,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Json,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tokentab: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Estimate(args) => estimate(args),
        Command::Prices(args) => prices(args),
    }
}

fn load_pricing(path: Option<&PathBuf>) -> Result<Loaded> {
    let text = match path {
        Some(path) => std::fs::read_to_string(path).with_context(|| format!("reading price list {}", path.display()))?,
        None => pricing_toml::DEFAULT_PRICING.to_string(),
    };
    Ok(pricing_toml::load(&text)?)
}

fn estimate(args: EstimateArgs) -> Result<()> {
    let loaded = load_pricing(args.pricing.as_ref())?;
    let projects_dir = match args.projects_dir {
        Some(dir) => dir,
        None => ClaudeCodeTranscripts::default_dir().context("cannot find the home directory; pass --projects-dir")?,
    };
    let filter = ScanFilter {
        since: args.since.as_deref().map(parse_utc_date).transpose()?,
        until: args.until.as_deref().map(parse_utc_date).transpose()?,
        project_substring: args.project,
    };
    let options = EstimateOptions {
        seats: args.seats,
        billing: match args.billing {
            BillingArg::Monthly => Billing::Monthly,
            BillingArg::Annual => Billing::Annual,
        },
        credits: match args.credits {
            CreditsArg::Apply => CreditsMode::Apply,
            CreditsArg::Ignore => CreditsMode::Ignore,
        },
        allowance_anchor: args.allowance_anchor.map(anchor_from_arg).transpose()?,
    };
    if args.seats == 0 {
        bail!("--seats must be at least 1");
    }

    let source = ClaudeCodeTranscripts::new(projects_dir);
    let scan = source.scan(&filter)?;
    let report = report::build(source.name(), scan, &loaded.prices, &loaded.plans, options)?;

    let renderer: Box<dyn Renderer> = match args.format {
        Format::Text => Box::new(TextRenderer),
        Format::Json => Box::new(JsonRenderer),
    };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    renderer.render(&report, &mut out)?;
    Ok(())
}

fn prices(args: PricesArgs) -> Result<()> {
    let loaded = load_pricing(args.pricing.as_ref())?;
    println!("price list as of {}", loaded.prices.as_of);
    println!("{:<28} {:>9} {:>9} {:>9} {:>9} {:>9}  prompt tokens over", "model", "input", "output", "cache rd", "write 5m", "write 1h");
    for model in &loaded.prices.models {
        for tier in &model.tiers {
            println!(
                "{:<28} {:>9.3} {:>9.3} {:>9.3} {:>9.3} {:>9.3}  {}",
                model.id,
                tier.input.usd_per_mtok(),
                tier.output.usd_per_mtok(),
                tier.cache_read.usd_per_mtok(),
                tier.cache_write_5m.usd_per_mtok(),
                tier.cache_write_1h.usd_per_mtok(),
                tier.prompt_tokens_over
            );
        }
    }
    println!();
    println!("allowance anchor {} per Pro seat per month", loaded.plans.allowance_anchor);
    println!("{:<16} {:<18} {:>10} {:>10} {:>5} {:>9} {:>10} {:>9}", "plan", "kind", "monthly", "annual", "min", "allow x", "credits", "pool cap");
    for plan in &loaded.plans.plans {
        println!(
            "{:<16} {:<18} {:>10} {:>10} {:>5} {:>9.1} {:>10} {:>9}",
            plan.id,
            format!("{:?}", plan.kind),
            plan.seat_monthly.map(|money| money.to_string()).unwrap_or_else(|| "-".into()),
            plan.seat_annual.map(|money| money.to_string()).unwrap_or_else(|| "-".into()),
            plan.min_seats,
            plan.allowance_multiple,
            plan.api_credits_per_seat,
            plan.api_credits_pool_cap.map(|money| money.to_string()).unwrap_or_else(|| "-".into()),
        );
    }
    Ok(())
}

fn parse_utc_date(text: &str) -> Result<jiff::Timestamp> {
    let date: jiff::civil::Date = text.parse().with_context(|| format!("{text}: expected YYYY-MM-DD"))?;
    Ok(date.to_zoned(jiff::tz::TimeZone::UTC).with_context(|| format!("{text}: not a valid UTC date"))?.timestamp())
}

fn anchor_from_arg(usd: f64) -> Result<Money> {
    if !usd.is_finite() || usd < 0.0 {
        bail!("--allowance-anchor must be a non-negative number of dollars");
    }
    Ok(Money::from_usd(usd))
}
