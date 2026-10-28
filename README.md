# tokentab

Replays local AI coding transcripts through a price list and compares what the same usage would have cost on each plan: API pay-as-you-go, Pro, Max 5x, Max 20x, Team Standard and Premium seats, and Enterprise.

The first source adapter reads Claude Code's local transcripts. The pricing, plan and pattern logic is independent of where the usage came from, so an analytics-API export or another vendor's logs can be added as a second adapter without touching the domain.

## Install

```sh
cargo install --path .
```

Rust 1.88 or later.

## Use

```sh
tokentab estimate                       # scans ~/.claude/projects
tokentab estimate --since 2026-09-01 --until 2026-10-01
tokentab estimate --project acme-api    # only requests whose working directory contains "acme-api"
tokentab estimate --seats 5 --billing annual
tokentab estimate --credits ignore      # do not offset SDK usage with bundled API credits
tokentab estimate --allowance-anchor 90 # what if a Pro seat is worth $90 of API usage a month
tokentab estimate --format json
tokentab prices                         # the compiled-in price list and plan catalog
```

`--seats` takes 1 to 10,000 and `--allowance-anchor` takes 0 to 100,000 dollars.

`--pricing path.toml` replaces the compiled-in price list. The file format is `pricing/anthropic.toml`.

## What the report contains

1. Scan counts: files seen, lines read, unparseable lines, assistant lines, requests before and after deduplication, requests filtered out, and every request the price list could not price, by reason. A run that finds no files or prices nothing exits non-zero instead of printing a clean zero.
2. Usage by month at API rates, split into interactive and headless (SDK) usage.
3. One row per plan: seats billed, average month, peak month, and the total over the range; then a month-by-plan table of what each month would have cost on each plan; then the assumptions each number rests on.
4. Usage patterns: the most expensive rolling 5-hour window and 7-day window, active days, sessions, sidechain (subagent) share, cost per model, per entry point and per project, and cache statistics with a no-cache counterfactual.
5. The assumptions in force: price list date, allowance anchor, seats, billing cycle, credits mode.

## How the numbers are computed

Every assistant record in a transcript carries the API usage for one message. A streamed message is written once per content block with the same message id and a growing output token count, so the scanner keeps the last line per message id and reports how many duplicates it collapsed.

Each request is priced at the published per-token rates for its model: base input, output, cache reads, and cache writes at the 5-minute or 1-hour rate the transcript records. Models from Claude 4.6 on bill the full 1M context at one rate. Haiku 5.5 is priced by prompt length and switches tier when input plus cache reads plus cache writes exceeds 100,000 tokens. Model ids with a snapshot date suffix map to their canonical id. Synthetic messages, requests without a model, and models absent from the price list are counted and shown, never priced as zero.

Money is held in whole nanodollars. Every published price per million tokens is a whole number of nanodollars per token, so totals over billions of tokens are exact.

Plans are priced from `pricing/anthropic.toml`:

- API pay-as-you-go: every token at API rates, no fee.
- Pro, Max and Team seats: a seat fee buys an allowance; interactive usage above it is extra usage at API rates. Anthropic publishes plan ratios (Max is 5x or 20x Pro, a Premium seat is 5x a Standard seat) but no dollar or token limit, so the allowance rests on one assumption, `allowance_anchor_usd`: the API-equivalent usage one Pro seat gets in a month. The report prints each plan's average month with the anchor halved and doubled so the reader can see how much rides on it.
- Max and Team include monthly Claude Platform API credits that cover the API, the Agent SDK and headless runs but not interactive Claude Code. The estimate applies them to SDK-entrypoint usage only, up to the pooled cap, and `--credits ignore` turns that off.
- Enterprise: seat fee plus every token at API rates, no allowance.

Seat minimums are enforced and noted: Team bills at least 2 seats, Enterprise at least 20. The transcript is one seat's usage; additional seats carry their fee and nothing else.

Months are UTC calendar months. Every month from the start of the range to its end gets a row, idle months included, so seat plans pay for them; a month holding only unpriced requests gets none. A month is partial when the range starts or ends inside it (at `--since` and `--until`, or at the first and last priced request when they are not given); partial months are billed in full in the totals but left out of the average and peak month, unless every month is partial, and the report names them. The peak 5-hour and 7-day windows show the heaviest stretch of usage, which is where a rolling plan limit would bind first.

## Layout

```
src/domain      money, usage events, pricing, plans, patterns, report
src/ports       UsageSource (inbound) and Renderer (outbound) traits
src/adapters    claude_code transcript reader, TOML price list loader, text and JSON renderers
pricing/        anthropic.toml, the compiled-in price list and plan catalog with its as-of date
tests/fixtures  small transcripts covering streamed duplicates, synthetic turns, API errors,
                both cache TTLs, the Haiku 5.5 tier switch, a dated model id and an unknown model
```

## Updating prices

Edit `pricing/anthropic.toml`, bump `as_of`, and rebuild. The loader rejects unknown fields, negative prices, duplicate ids and plans without a seat price.

## Development

Tool versions and tasks live in `mise.toml`.

```sh
mise install        # rust with rustfmt and clippy, mr-boxington, lefthook, zizmor, jactionlint, gh
mise run hooks      # lefthook: fmt on commit; clippy, tests and workflow audits on push
mise run ci         # everything the ci workflow runs
mise run estimate -- --since 2026-09-01
```

Cargo runs through mbx, a shared compiler-output cache that mise wires in through the `mr_boxington` flag on the rust pin, so `target` is a link into the cache's own directory for this checkout. The ci workflow restores the mbx store and the cargo registry on every run and saves them only after a passing push to the default branch, so a pull request never writes compiler outputs another run reads.
