use nu_protocol::{ShellError, Value, shell_error::generic::GenericError};

/// Converts a nu value into a serde value so it can be encoded in any serde format. Supports
/// bool, int, float, string, nothing, list and record; anything else is a type error that
/// names `flag` (the option the value came from, such as `--opts`).
pub(crate) fn to_serde_value(value: &Value, flag: &str) -> Result<serde_json::Value, ShellError> {
    let json = match value {
        Value::Bool { val, .. } => serde_json::Value::Bool(*val),
        Value::Int { val, .. } => serde_json::Value::from(*val),
        Value::Float { val, .. } => serde_json::Number::from_f64(*val)
            .map(serde_json::Value::Number)
            .ok_or_else(|| unsupported(value, flag, "a finite float"))?,
        Value::String { val, .. } => serde_json::Value::String(val.clone()),
        Value::Nothing { .. } => serde_json::Value::Null,
        Value::List { vals, .. } => serde_json::Value::Array(
            vals.iter()
                .map(|v| to_serde_value(v, flag))
                .collect::<Result<_, _>>()?,
        ),
        Value::Record { val, .. } => serde_json::Value::Object(
            val.iter()
                .map(|(k, v)| Ok((k.clone(), to_serde_value(v, flag)?)))
                .collect::<Result<_, ShellError>>()?,
        ),
        _ => {
            return Err(unsupported(
                value,
                flag,
                "bool, int, float, string, nothing, list or record",
            ));
        }
    };
    Ok(json)
}

fn unsupported(value: &Value, flag: &str, expected: &str) -> ShellError {
    ShellError::Generic(GenericError::new(
        format!("{flag} expects {expected}"),
        format!("found {}", value.get_type()),
        value.span(),
    ))
}
