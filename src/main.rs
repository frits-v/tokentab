use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};

use tokentab::adapters::claude_code::ClaudeCodeTranscripts;
use tokentab::adapters::pricing_toml::{self, Loaded};
use tokentab::adapters::render_json::JsonRenderer;
use tokentab::adapters::render_text::TextRenderer;
use tokentab::domain::money::Money;
use tokentab::domain::plans::{Billing, CreditsMode, EstimateOptions};
use tokentab::domain::report::{self, RequestedRange};
use tokentab::domain::usage::ScanFilter;
use tokentab::ports::renderer::Renderer;
use tokentab::ports::usage_source::UsageSource;

#[derive(Parser)]
#[command(
    name = "tokentab",
    version,
    about = "Replays local AI coding transcripts through a price list and compares what they would have cost on each plan"
)]
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
    /// Seats to bill for Team and Enterprise (1 to 10000); plan minimums are applied and noted.
    #[arg(
        long,
        default_value_t = 1,
        value_parser = clap::value_parser!(u32).range(1..=i64::from(pricing_toml::MAX_SEATS)),
        allow_negative_numbers = true
    )]
    seats: u32,
    /// Seat price to use: monthly or annual billing.
    #[arg(long, value_enum, default_value_t = BillingArg::Monthly)]
    billing: BillingArg,
    /// Whether bundled Max/Team API credits offset SDK and headless usage.
    #[arg(long, value_enum, default_value_t = CreditsArg::Apply)]
    credits: CreditsArg,
    /// API-equivalent dollars one Pro seat gets per month; every allowance is a multiple of it.
    #[arg(long, allow_negative_numbers = true)]
    allowance_anchor: Option<f64>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
}

#[derive(clap::Args)]
struct PricesArgs {
    /// Price list TOML (default: the list compiled into the binary).
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

/// Exit status a shell reports for a process killed by SIGPIPE (128 + 13).
const EXIT_BROKEN_PIPE: u8 = 141;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if is_broken_pipe(&error) => ExitCode::from(EXIT_BROKEN_PIPE),
        Err(error) => {
            eprintln!("tokentab: {error:#}");
            ExitCode::from(2)
        }
    }
}

/// The reader of stdout went away (`tokentab prices | head`): stop quietly.
fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|io_error| io_error.kind() == io::ErrorKind::BrokenPipe)
    })
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
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("reading price list {}", path.display()))?,
        None => pricing_toml::DEFAULT_PRICING.to_string(),
    };
    Ok(pricing_toml::load(&text)?)
}

fn estimate(args: EstimateArgs) -> Result<()> {
    let loaded = load_pricing(args.pricing.as_ref())?;
    let projects_dir = match args.projects_dir {
        Some(dir) => dir,
        None => ClaudeCodeTranscripts::default_dir()
            .context("cannot find the home directory; pass --projects-dir")?,
    };
    let since = args.since.as_deref().map(parse_utc_date).transpose()?;
    let until = args.until.as_deref().map(parse_utc_date).transpose()?;
    if let (Some(since), Some(until)) = (since, until)
        && since >= until
    {
        bail!("--since must be before --until");
    }
    let filter = ScanFilter {
        since,
        until,
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

    let source = ClaudeCodeTranscripts::new(projects_dir);
    let scan = source.scan(&filter)?;
    let report = report::build(
        source.name(),
        scan,
        &loaded.prices,
        &loaded.plans,
        options,
        RequestedRange { since, until },
    )?;

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
    let stdout = io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "price list as of {}", loaded.prices.as_of)?;
    writeln!(
        out,
        "{:<20} {:>10} {:>10} {:>10} {:>10} {:>10}  prompt tokens over",
        "model", "input", "output", "cache read", "write 5m", "write 1h"
    )?;
    for model in &loaded.prices.models {
        for tier in &model.tiers {
            writeln!(
                out,
                "{:<20} {:>10.3} {:>10.3} {:>10.3} {:>10.3} {:>10.3}  {}",
                model.id,
                tier.input.usd_per_mtok(),
                tier.output.usd_per_mtok(),
                tier.cache_read.usd_per_mtok(),
                tier.cache_write_5m.usd_per_mtok(),
                tier.cache_write_1h.usd_per_mtok(),
                tier.prompt_tokens_over
            )?;
        }
    }
    writeln!(out)?;
    writeln!(
        out,
        "allowance anchor {} per Pro seat per month",
        loaded.plans.allowance_anchor
    )?;
    writeln!(
        out,
        "{:<16} {:<15} {:>10} {:>10} {:>5} {:>5} {:>11} {:>10} {:>9}",
        "plan", "kind", "monthly", "annual", "min", "max", "allowance x", "credits", "pool cap"
    )?;
    let or_dash = |value: Option<String>| value.unwrap_or_else(|| "-".to_string());
    for plan in &loaded.plans.plans {
        writeln!(
            out,
            "{:<16} {:<15} {:>10} {:>10} {:>5} {:>5} {:>11.1} {:>10} {:>9}",
            plan.id,
            plan.kind.label(),
            or_dash(plan.seat_monthly.map(|money| money.to_string())),
            or_dash(plan.seat_annual.map(|money| money.to_string())),
            plan.min_seats,
            or_dash(plan.max_seats.map(|seats| seats.to_string())),
            plan.allowance_multiple,
            plan.api_credits_per_seat,
            or_dash(plan.api_credits_pool_cap.map(|money| money.to_string())),
        )?;
    }
    Ok(())
}

fn parse_utc_date(text: &str) -> Result<jiff::Timestamp> {
    let date: jiff::civil::Date = text
        .parse()
        .map_err(|_| anyhow!("expected YYYY-MM-DD, got {text}"))?;
    let zoned = date
        .to_zoned(jiff::tz::TimeZone::UTC)
        .map_err(|_| anyhow!("{text} is outside the supported date range"))?;
    Ok(zoned.timestamp())
}

fn anchor_from_arg(usd: f64) -> Result<Money> {
    if !usd.is_finite() || !(0.0..=pricing_toml::MAX_USD_AMOUNT).contains(&usd) {
        bail!(
            "--allowance-anchor must be between 0 and {} dollars",
            pricing_toml::MAX_USD_AMOUNT
        );
    }
    Ok(Money::from_usd(usd))
}
