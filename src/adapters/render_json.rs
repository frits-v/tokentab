use std::io::{self, Write};

use crate::domain::report::Report;
use crate::ports::renderer::Renderer;

pub struct JsonRenderer;

impl Renderer for JsonRenderer {
    fn render(&self, report: &Report, out: &mut dyn Write) -> io::Result<()> {
        todo!("serde_json::to_writer_pretty plus a trailing newline")
    }
}
