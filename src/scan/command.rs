use nu_plugin::{EngineInterface, EvaluatedCall, PluginCommand};
use nu_protocol::{
    Category, DataSource, Example, LabeledError, PipelineData, PipelineMetadata, ShellError,
    Signature, Span, Spanned, SyntaxShape, Type, Value, shell_error::generic::GenericError,
};

use crate::{
    PolarsPlugin,
    command::core::resource::Resource,
    nu_serde::to_serde_value,
    values::{CustomValueSupport, NuLazyFrame, PolarsPluginType},
};

/// `polars_dyn open <source> [--format <name>] [--opts <record>]` → LazyFrame.
///
/// The output carries the source string as `DataSource::FilePath`, which `polars_dyn save`
/// checks to refuse writing into the file a frame is still being read from.
#[derive(Clone)]
pub struct Open;

impl PluginCommand for Open {
    type Plugin = PolarsPlugin;

    fn name(&self) -> &str {
        "polars_dyn open"
    }

    fn description(&self) -> &str {
        "Opens a source as a lazy dataframe through one of the registered scan sources."
    }

    fn signature(&self) -> Signature {
        Signature::build(self.name())
            .required(
                "source",
                SyntaxShape::String,
                "File path, cloud URL, or whatever the scan source reads.",
            )
            .named(
                "format",
                SyntaxShape::String,
                "Registered scan source name. If omitted, derive from the suffix of the source.",
                Some('f'),
            )
            .named(
                "opts",
                SyntaxShape::Record(Vec::new().into()),
                "Options for the scan source, passed through as JSON.",
                Some('o'),
            )
            .input_output_type(Type::Any, PolarsPluginType::NuLazyFrame.into())
            .category(Category::Custom("dataframe".into()))
    }

    fn examples(&self) -> Vec<Example<'_>> {
        vec![
            Example {
                description: "Open a parquet file",
                example: "polars_dyn open data.parquet",
                result: None,
            },
            Example {
                description: "Open a csv file without a header row",
                example: "polars_dyn open data.csv --opts {has_header: false}",
                result: None,
            },
            Example {
                description: "Open a file whose suffix does not tell the format",
                example: "polars_dyn open data.txt --format csv",
                result: None,
            },
        ]
    }

    fn run(
        &self,
        plugin: &Self::Plugin,
        engine: &EngineInterface,
        call: &EvaluatedCall,
        _input: PipelineData,
    ) -> Result<PipelineData, LabeledError> {
        command(plugin, engine, call).map_err(LabeledError::from)
    }
}

fn command(
    plugin: &PolarsPlugin,
    engine: &EngineInterface,
    call: &EvaluatedCall,
) -> Result<PipelineData, ShellError> {
    let spanned_source: Spanned<String> = call.req(0)?;
    let format: Option<Spanned<String>> = call.get_flag("format")?;
    let opts: Option<Value> = call.get_flag("opts")?;

    let source = resolve_source(plugin, engine, &spanned_source)?;
    let scan_source = match &format {
        Some(name) => plugin
            .scan_registry
            .find_by_name(&name.item)
            .ok_or_else(|| no_source_error(plugin, &name.item, name.span))?,
        None => plugin
            .scan_registry
            .find_by_suffix(&source)
            .ok_or_else(|| no_source_error(plugin, &source, spanned_source.span))?,
    };
    let opts = match opts {
        Some(record) => serde_json::to_vec(&to_json(&record)?).map_err(|e| {
            ShellError::Generic(GenericError::new_internal(
                format!("Could not encode --opts as JSON: {e}"),
                "",
            ))
        })?,
        None => Vec::new(),
    };

    let lazy = scan_source.scan(&source, &opts).map_err(|e| {
        ShellError::Generic(GenericError::new(
            format!("{} scan error", scan_source.name()),
            e.to_string(),
            spanned_source.span,
        ))
    })?;
    let value = NuLazyFrame::from(lazy).cache_and_to_value(plugin, engine, call.head)?;
    let metadata = PipelineMetadata::default()
        .with_data_source(DataSource::FilePath(spanned_source.item.into()));
    Ok(PipelineData::value(value, Some(metadata)))
}

/// The source string a [`ScanSource`](super::ScanSource) receives. A URL with any scheme
/// (`s3://`, `ssh://`, ...) is passed through untouched; everything else is a local path made
/// absolute against the engine's current directory. The scheme is checked here rather than by
/// `Resource`, whose `PlRefPath::has_scheme` knows only the cloud schemes polars reads itself.
fn resolve_source(
    plugin: &PolarsPlugin,
    engine: &EngineInterface,
    spanned_source: &Spanned<String>,
) -> Result<String, ShellError> {
    if has_url_scheme(&spanned_source.item) {
        return Ok(spanned_source.item.clone());
    }
    Ok(Resource::new(plugin, engine, spanned_source)?.as_string())
}

/// `<scheme>://` per RFC 3986: a letter, then letters, digits, `+`, `-` or `.`.
fn has_url_scheme(source: &str) -> bool {
    let Some((scheme, _)) = source.split_once("://") else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn no_source_error(plugin: &PolarsPlugin, what: &str, span: Span) -> ShellError {
    let names = plugin.scan_registry.names().collect::<Vec<_>>().join(", ");
    ShellError::Generic(
        GenericError::new(
            format!("No scan source for `{what}`"),
            format!("registered: {names}"),
            span,
        )
        .with_help("Pass --format with one of the registered names"),
    )
}

/// Encodes a `--opts` record as JSON. Supports bool, int, float, string, nothing, list and
/// record; anything else is a type error.
fn to_json(value: &Value) -> Result<serde_json::Value, ShellError> {
    to_serde_value(value, "--opts")
}
