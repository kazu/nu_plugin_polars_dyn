use polars::prelude::{PolarsError, PolarsResult, polars_bail};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

/// Parses the `--opts` bytes as a JSON object. Empty bytes are an empty object.
///
/// ```
/// # fn main() -> polars::prelude::PolarsResult<()> {
/// use nu_plugin_polars::scan::parse_opts;
/// assert!(parse_opts(b"")?.is_empty());
/// assert_eq!(parse_opts(br#"{"batch_size": 10}"#)?["batch_size"], 10);
/// assert!(parse_opts(b"[1]").is_err());
/// # Ok(())
/// # }
/// ```
pub fn parse_opts(opts: &[u8]) -> PolarsResult<Map<String, Value>> {
    if opts.is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_slice(opts).map_err(json_error)? {
        Value::Object(map) => Ok(map),
        other => polars_bail!(ComputeError: "opts must be a record, got {other}"),
    }
}

/// Deserializes `T` from `defaults` with `opts` overlaid on top.
///
/// `opts` is merged into the JSON form of `defaults` object by object, so a record only needs
/// the fields it changes. A key that `defaults` does not have is an error, since the option
/// structs of polars ignore unknown fields silently.
///
/// ```
/// # fn main() -> polars::prelude::PolarsResult<()> {
/// use nu_plugin_polars::scan::overlay_opts;
/// use polars_io::csv::read::CsvReadOptions;
/// let opts = serde_json::json!({"has_header": false, "parse_options": {"separator": 59}});
/// let options = overlay_opts(CsvReadOptions::default(), &opts)?;
/// assert!(!options.has_header);
/// assert_eq!(options.parse_options.separator, b';');
/// assert!(overlay_opts(CsvReadOptions::default(), &serde_json::json!({"has_hedar": false})).is_err());
/// # Ok(())
/// # }
/// ```
pub fn overlay_opts<T>(defaults: T, opts: &Value) -> PolarsResult<T>
where
    T: Serialize + DeserializeOwned,
{
    let mut base = serde_json::to_value(defaults).map_err(json_error)?;
    merge(&mut base, opts, "")?;
    serde_json::from_value(base).map_err(json_error)
}

fn merge(base: &mut Value, over: &Value, path: &str) -> PolarsResult<()> {
    let (Value::Object(base_map), Value::Object(over_map)) = (&mut *base, over) else {
        *base = over.clone();
        return Ok(());
    };
    for (key, value) in over_map {
        let Some(slot) = base_map.get_mut(key) else {
            polars_bail!(ComputeError: "unknown option `{path}{key}`");
        };
        merge(slot, value, &format!("{path}{key}."))?;
    }
    Ok(())
}

fn json_error(e: serde_json::Error) -> PolarsError {
    PolarsError::ComputeError(format!("opts: {e}").into())
}
