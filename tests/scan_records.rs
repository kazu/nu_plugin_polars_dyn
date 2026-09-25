//! A `Records` source handed straight to a built-in format, without a plugin in between.

use std::sync::Arc;

use nu_plugin_polars::scan::{Bytes, Records, ScanRegistry};
use nu_protocol::Span;
use polars::prelude::{PolarsResult, col};

/// A source that holds no unit, and says so.
struct NoUnits;

impl Records for NoUnits {
    fn read_unit(&self, _index: usize, _dst: &mut Vec<u8>) -> PolarsResult<bool> {
        Ok(false)
    }

    fn count_units(&self) -> Option<usize> {
        Some(0)
    }
}

/// With no unit to read, the frame is empty and still narrowed to the columns the query asks for.
#[test]
fn records_without_a_unit_give_an_empty_frame_of_the_asked_columns()
-> Result<(), Box<dyn std::error::Error>> {
    let chain = ScanRegistry::new(&[])?.resolve("x.ndjson", None, Span::unknown())?;
    let opts = br#"{"schema": {"fields": {"a": "Int64", "b": "String"}, "metadata": null}}"#;
    let df = chain
        .last
        .scan(Bytes::Records(Arc::new(NoUnits)), opts)?
        .select([col("a")])
        .collect()?;
    assert_eq!(df.height(), 0);
    assert_eq!(df.get_column_names(), ["a"]);
    Ok(())
}
