use std::io::{self, Write};

use crate::domain::report::Report;
use crate::ports::renderer::Renderer;

pub struct JsonRenderer;

impl Renderer for JsonRenderer {
    fn render(&self, report: &Report, out: &mut dyn Write) -> io::Result<()> {
        serde_json::to_writer_pretty(&mut *out, report).map_err(io::Error::other)?;
        out.write_all(b"\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::report::fixtures::sample_report;
    use crate::domain::usage::Granularity;

    #[test]
    fn json_report_parses_and_ends_with_newline() {
        let mut buffer = Vec::new();
        JsonRenderer
            .render(&sample_report(Granularity::Day), &mut buffer)
            .expect("rendering into a Vec cannot fail");
        let text = String::from_utf8(buffer).expect("serde_json writes UTF-8");
        assert!(text.ends_with("}\n"));
        let value: serde_json::Value = serde_json::from_str(&text).expect("output is valid JSON");
        assert_eq!(value["scan"]["files_seen"], 3);
        assert_eq!(value["unpriced"]["synthetic"], 1);
        assert_eq!(value["months"].as_array().map(Vec::len), Some(2));
        assert_eq!(value["plans"][1]["plan_id"], "pro");
        assert!(value["patterns"]["window_5h"].is_null());
        assert_eq!(value["allowance_anchor"], 60.0);
    }
}
