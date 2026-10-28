//! Plain-text report for a terminal, at most 100 columns wide, no colour.
//!
//! Sections in order:
//! 1. Scan: source, files seen, lines read, unparseable lines, assistant lines,
//!    requests before dedup, duplicates collapsed, filtered out, priced, unpriced by reason.
//! 2. Usage by month: month, requests, interactive API-equivalent, headless API-equivalent, total.
//! 3. Plans: one row per plan with average month, peak month, total over the range,
//!    then per-plan notes and the sensitivity line (anchor halved / doubled).
//! 4. Patterns: peak 5-hour window and peak 7-day window (or "unavailable at day
//!    granularity"), active days, sessions, sidechain share, per-model table with
//!    tokens and cost, per-entrypoint table, top projects, cache stats with the
//!    no-cache counterfactual and the saving.
//! 5. Assumptions: pricing as-of date, allowance anchor, seats and billing, credits mode.

use std::io::{self, Write};

use crate::domain::report::Report;
use crate::ports::renderer::Renderer;

pub struct TextRenderer;

impl Renderer for TextRenderer {
    fn render(&self, report: &Report, out: &mut dyn Write) -> io::Result<()> {
        todo!("see the module doc")
    }
}
