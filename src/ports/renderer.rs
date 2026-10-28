use std::io::{self, Write};

use crate::domain::report::Report;

/// Outbound port. Renders a finished report to a writer.
pub trait Renderer {
    fn render(&self, report: &Report, out: &mut dyn Write) -> io::Result<()>;
}
