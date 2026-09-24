use std::collections::HashMap;

use nu_plugin::{EngineInterface, EvaluatedCall, PluginCommand};
use nu_protocol::{
    Category, DataSource, Example, LabeledError, PipelineData, PipelineMetadata, ShellError,
    Signature, Span, Spanned, SyntaxShape, Type, Value, shell_error::generic::GenericError,
};
use polars::prelude::{LazyFrame, PolarsError, UnionArgs, concat};

use crate::{
    PolarsPlugin,
    command::core::resource::Resource,
    nu_serde::to_serde_value,
    scan::{Chain, ScanSource, url_scheme},
    values::{CustomValueSupport, NuLazyFrame, PolarsPluginType},
};

/// `polars_dyn open <source> [--format <a,b,c>] [--opts <record>]` → LazyFrame.
///
/// A local path whose absolute form, the current directory included, has a glob opens every
/// match through the same chain and stacks the frames in the order of the paths as strings; a
/// URL with a scheme is not expanded.
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
        "Opens a source as a lazy dataframe through a chain of the registered scan sources."
    }

    fn signature(&self) -> Signature {
        Signature::build(self.name())
            .required(
                "source",
                SyntaxShape::String,
                "File path or glob, or a URL whose scheme a registered scan source opens.",
            )
            .named(
                "format",
                SyntaxShape::String,
                "Registered scan source names, comma-separated, in place of the suffixes of the source.",
                Some('f'),
            )
            .named(
                "opts",
                SyntaxShape::Record(Vec::new().into()),
                "Options per scan source, keyed by its name, each passed through as JSON.",
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
                example: "polars_dyn open data.csv --opts {csv: {has_header: false}}",
                result: None,
            },
            Example {
                description: "Open every ndjson file of a directory as one frame",
                example: "polars_dyn open 'logs/*.jsonl'",
                result: None,
            },
            Example {
                description: "Open a file whose suffix does not tell the format",
                example: "polars_dyn open data.txt --format csv",
                result: None,
            },
            Example {
                description: "Name the chain when the suffixes do not: a seekable zstd of ndjson",
                example: "polars_dyn open dump.bin --format seek-zst,ndjson",
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
    let opts: Option<Spanned<Value>> = call.get_flag("opts")?;

    let source = resolve_source(plugin, engine, &spanned_source)?;
    let chain = plugin.scan_registry.resolve(
        &source,
        format.as_ref().map(|f| f.item.as_str()),
        format.as_ref().map_or(spanned_source.span, |f| f.span),
    )?;
    let opts = split_opts(opts.as_ref(), &chain)?;

    let frames = expand_local_glob(&spanned_source, source)?
        .iter()
        .map(|path| run_chain(&chain, path, &opts, spanned_source.span))
        .collect::<Result<Vec<_>, _>>()?;
    let lazy = stack_frames(frames, spanned_source.span)?;
    let value = NuLazyFrame::from(lazy).cache_and_to_value(plugin, engine, call.head)?;
    let metadata = PipelineMetadata::default()
        .with_data_source(DataSource::FilePath(spanned_source.item.into()));
    Ok(PipelineData::value(value, Some(metadata)))
}

/// Opens with the head, wraps with the middle and scans with the last of `chain`, each given
/// its own entry of `opts`. An error names the source that raised it.
fn run_chain(
    chain: &Chain,
    source: &str,
    opts: &HashMap<&str, Vec<u8>>,
    span: Span,
) -> Result<LazyFrame, ShellError> {
    let opts_of = |scan: &dyn ScanSource| opts.get(scan.name()).map_or(&[][..], Vec::as_slice);
    let scan_error = |scan: &dyn ScanSource, e: PolarsError| {
        ShellError::Generic(GenericError::new(
            format!("{} scan error", scan.name()),
            e.to_string(),
            span,
        ))
    };

    let mut bytes = chain
        .head
        .open(source, opts_of(chain.head))
        .map_err(|e| scan_error(chain.head, e))?;
    for scan in &chain.middle {
        bytes = scan
            .wrap(bytes, opts_of(*scan))
            .map_err(|e| scan_error(*scan, e))?;
    }
    chain
        .last
        .scan(bytes, opts_of(chain.last))
        .map_err(|e| scan_error(chain.last, e))
}

/// The string the head of the chain opens. A URL with any scheme (`ssh://`, `file://`, ...) is
/// passed through untouched; everything else is a local path made absolute against the engine's
/// current directory. The scheme is checked here rather than by `Resource`, whose
/// `PlRefPath::has_scheme` knows only the cloud schemes polars reads itself.
fn resolve_source(
    plugin: &PolarsPlugin,
    engine: &EngineInterface,
    spanned_source: &Spanned<String>,
) -> Result<String, ShellError> {
    if url_scheme(&spanned_source.item).is_some() {
        return Ok(spanned_source.item.clone());
    }
    Ok(Resource::new(plugin, engine, spanned_source)?.as_string())
}

/// The files `source`, the string [`resolve_source`] made of `spanned_source`, names: when
/// `source` is a local path with a glob metacharacter (`*`, `?`, `[`), the matches that are
/// neither a directory nor empty, sorted as strings, else `source` alone. These are the rules
/// polars' own path scan expands a glob by. A glob without a match is an error, as is a match
/// whose metadata cannot be read.
fn expand_local_glob(
    spanned_source: &Spanned<String>,
    source: String,
) -> Result<Vec<String>, ShellError> {
    let item = &spanned_source.item;
    if url_scheme(&source).is_some() || !source.contains(['*', '?', '[']) {
        return Ok(vec![source]);
    }
    let glob_error = |detail: String| {
        ShellError::Generic(GenericError::new(
            format!("Could not expand `{item}`"),
            detail,
            spanned_source.span,
        ))
    };
    let mut files = Vec::new();
    for path in glob::glob(&source).map_err(|e| glob_error(e.to_string()))? {
        let path = path.map_err(|e| glob_error(e.to_string()))?;
        let metadata = path.metadata().map_err(|e| glob_error(e.to_string()))?;
        if !metadata.is_dir() && metadata.len() > 0 {
            files.push(path.to_string_lossy().into_owned());
        }
    }
    files.sort_unstable();
    if files.is_empty() {
        return Err(ShellError::Generic(GenericError::new(
            format!("No file matches `{item}`"),
            "",
            spanned_source.span,
        )));
    }
    Ok(files)
}

/// Stacks the frames of the files of a source in their order with polars' `concat`; frames whose
/// columns differ fail when the result is collected. A single frame is returned as it is.
fn stack_frames(frames: Vec<LazyFrame>, span: Span) -> Result<LazyFrame, ShellError> {
    match <[LazyFrame; 1]>::try_from(frames) {
        Ok([frame]) => Ok(frame),
        Err(frames) => concat(frames, UnionArgs::default()).map_err(|e| {
            ShellError::Generic(GenericError::new(
                "Could not concatenate the files",
                e.to_string(),
                span,
            ))
        }),
    }
}

/// Cuts the `--opts` record into one JSON blob per scan source of `chain`, keyed by name. A key
/// that names no source of the chain is an error, as is a value that is not a record. Supports
/// bool, int, float, string, nothing, list and record inside; anything else is a type error.
fn split_opts<'a>(
    opts: Option<&'a Spanned<Value>>,
    chain: &Chain,
) -> Result<HashMap<&'a str, Vec<u8>>, ShellError> {
    let mut split = HashMap::new();
    let Some(opts) = opts else {
        return Ok(split);
    };
    let record = opts.item.as_record()?;
    for (name, value) in record.iter() {
        if !chain.names().any(|n| n == name) {
            return Err(ShellError::Generic(GenericError::new(
                format!("unknown scan `{name}` in --opts"),
                format!(
                    "the chain is: {}",
                    chain.names().collect::<Vec<_>>().join(", ")
                ),
                opts.span,
            )));
        }
        if !matches!(value, Value::Record { .. }) {
            return Err(ShellError::Generic(GenericError::new(
                format!("--opts `{name}` must be a record"),
                format!("got {}", value.get_type()),
                value.span(),
            )));
        }
        let json = serde_json::to_vec(&to_serde_value(value, "--opts")?).map_err(|e| {
            ShellError::Generic(GenericError::new_internal(
                format!("Could not encode --opts as JSON: {e}"),
                "",
            ))
        })?;
        split.insert(name.as_str(), json);
    }
    Ok(split)
}
