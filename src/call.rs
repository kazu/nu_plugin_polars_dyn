use nu_path::expand_path_with;
use nu_plugin::{EngineInterface, EvaluatedCall, PluginCommand};
use nu_protocol::{
    Category, Example, LabeledError, PipelineData, ShellError, Signature, Spanned, SyntaxShape,
    Type, Value, shell_error::generic::GenericError,
};
use polars::prelude::{Expr, FunctionExpr};
use polars_plan::prelude::{CastingRules, FunctionFlags, FunctionOptions};

use crate::{
    PolarsPlugin,
    nu_serde::to_serde_value,
    values::{CustomValueSupport, NuExpression, PolarsPluginType},
};

/// `polars_dyn call <lib> <symbol> ...args [--kwargs] [--elementwise] [--returns-scalar]
/// [--changes-length] [--cast-to-supertype] [--pass-name]` → Expr.
///
/// Builds the `FunctionExpr::FfiPlugin` node polars uses for expression plugins, the same way
/// `register_plugin_function` does in py-polars. `lib` is made absolute against the nu current
/// directory here, because polars opens it only when the plan runs. `--kwargs` is pickled, which
/// is what a `#[polars_expr]` plugin expects.
#[derive(Clone)]
pub struct Call;

impl PluginCommand for Call {
    type Plugin = PolarsPlugin;

    fn name(&self) -> &str {
        "polars_dyn call"
    }

    fn description(&self) -> &str {
        "Calls a function exported by a polars expression plugin (a shared library)."
    }

    fn signature(&self) -> Signature {
        Signature::build(self.name())
            .required(
                "lib",
                SyntaxShape::Filepath,
                "Path to the shared library. A relative path is resolved against the current directory now, not at collect time.",
            )
            .required(
                "symbol",
                SyntaxShape::String,
                "Function name: the <symbol> part of the exported `_polars_plugin_<symbol>`.",
            )
            .rest(
                "args",
                SyntaxShape::Any,
                "Input expressions. A string is a column name.",
            )
            .named(
                "kwargs",
                SyntaxShape::Record(Vec::new().into()),
                "Keyword arguments, passed to the plugin as pickle bytes.",
                Some('k'),
            )
            .switch(
                "elementwise",
                "The function maps each row independently of the others.",
                None,
            )
            .switch(
                "returns-scalar",
                "The function returns one value per group.",
                None,
            )
            .switch(
                "changes-length",
                "The output length may differ from the input length.",
                None,
            )
            .switch(
                "cast-to-supertype",
                "Cast all inputs to their common supertype before calling.",
                None,
            )
            .switch(
                "pass-name",
                "Pass the input column name to the function (for `over` and `group-by`).",
                None,
            )
            .input_output_type(Type::Any, PolarsPluginType::NuExpression.into())
            .category(Category::Custom("expression".into()))
    }

    fn examples(&self) -> Vec<Example<'_>> {
        vec![
            Example {
                description: "Call `add` from a plugin on column a with a keyword argument",
                example: "[[a]; [1] [2]] | polars_dyn into-df | polars_dyn select (polars_dyn call ./libmy_plugin.so add a --kwargs {n: 10} --elementwise) | polars_dyn collect",
                result: None,
            },
            Example {
                description: "Call a two-input function without keyword arguments",
                example: "polars_dyn call plugins/libmy_plugin.so distance (polars_dyn col x) (polars_dyn col y)",
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
    let lib: Spanned<String> = call.req(0)?;
    let symbol: String = call.req(1)?;
    let args: Vec<Value> = call.rest(2)?;
    let kwargs: Option<Value> = call.get_flag("kwargs")?;

    let lib = expand_path_with(&lib.item, engine.get_current_dir()?, true)
        .to_str()
        .ok_or_else(|| {
            ShellError::Generic(GenericError::new(
                "Library path is not valid UTF-8",
                "polars needs the path as a string",
                lib.span,
            ))
        })?
        .to_owned();
    let input = NuExpression::extract_exprs(plugin, Value::list(args, call.head))?;
    let kwargs = match kwargs {
        Some(record) => to_pickle(&record)?,
        None => Vec::new(),
    };
    let flags = function_options(call)?;

    let expr = Expr::Function {
        input,
        function: FunctionExpr::FfiPlugin {
            flags,
            lib: lib.into(),
            symbol: symbol.into(),
            kwargs: kwargs.into(),
        },
    };
    NuExpression::from(expr).to_pipeline_data(plugin, engine, call.head)
}

/// Encodes a `--kwargs` record as pickle bytes, the format `#[polars_expr]` decodes.
fn to_pickle(record: &Value) -> Result<Vec<u8>, ShellError> {
    let value = to_serde_value(record, "--kwargs")?;
    serde_pickle::to_vec(&value, serde_pickle::SerOptions::new()).map_err(|e| {
        ShellError::Generic(GenericError::new(
            "Could not encode --kwargs as pickle",
            e.to_string(),
            record.span(),
        ))
    })
}

/// Maps the switches to polars' `FunctionOptions` exactly as py-polars'
/// `register_plugin_function` does.
fn function_options(call: &EvaluatedCall) -> Result<FunctionOptions, ShellError> {
    let mut flags = FunctionFlags::default();
    if call.has_flag("elementwise")? {
        flags.set_elementwise();
    }
    flags.set(
        FunctionFlags::LENGTH_PRESERVING,
        !call.has_flag("changes-length")?,
    );
    flags.set(
        FunctionFlags::PASS_NAME_TO_APPLY,
        call.has_flag("pass-name")?,
    );
    flags.set(
        FunctionFlags::RETURNS_SCALAR,
        call.has_flag("returns-scalar")?,
    );
    let cast_options = call
        .has_flag("cast-to-supertype")?
        .then(CastingRules::cast_to_supertypes);
    Ok(FunctionOptions {
        cast_options,
        flags,
        ..Default::default()
    })
}
