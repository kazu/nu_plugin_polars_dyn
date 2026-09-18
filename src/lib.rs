#![allow(clippy::result_large_err)]
use std::{
    cmp::Ordering,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::OnceLock,
};

use cache::cache_commands;
pub use cache::{Cache, Cacheable};
use command::{
    aggregation::aggregation_commands, boolean::boolean_commands,
    computation::computation_commands, core::core_commands, data::data_commands,
    datetime::datetime_commands, index::index_commands, integer::integer_commands,
    list::list_commands, selector::selector_commands, string::string_commands, stub::PolarsCmd,
};
use log::debug;
use nu_plugin::{EngineInterface, MsgPackSerializer, Plugin, PluginCommand, serve_plugin};

mod cache;
pub mod call;
pub mod dataframe;
mod nu_serde;
pub mod scan;
pub use dataframe::*;
use nu_protocol::{
    CustomValue, LabeledError, ShellError, Span, Spanned, Value, ast::Operator, casing::Casing,
    shell_error::generic::GenericError,
};
use scan::{ScanRegistry, ScanSource};
use values::CustomValueType;

use crate::values::PolarsPluginCustomValue;

pub struct PolarsPlugin {
    pub(crate) cache: Cache,
    gc_disabled: OnceLock<()>,
    pub(crate) scan_registry: ScanRegistry,
}

impl PolarsPlugin {
    /// Builds the plugin with the scan sources `polars_dyn open` can use. Fails when two
    /// sources share a name or a suffix.
    pub fn new(sources: &'static [&'static dyn ScanSource]) -> Result<Self, ShellError> {
        Ok(Self {
            cache: Cache::default(),
            gc_disabled: OnceLock::new(),
            scan_registry: ScanRegistry::new(sources)?,
        })
    }

    /// Turns the plugin GC off the first time the plugin holds an engine. A cached value lives
    /// until the engine reports the drop of every value the plugin handed out for it, so the
    /// engine must not stop the plugin while any value is cached.
    pub(crate) fn disable_gc_once(&self, engine: &EngineInterface) -> Result<(), ShellError> {
        if self.gc_disabled.get().is_some() {
            return Ok(());
        }
        engine.set_gc_disabled(true)?;
        let _ = self.gc_disabled.set(());
        Ok(())
    }
}

impl Plugin for PolarsPlugin {
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").into()
    }

    fn commands(&self) -> Vec<Box<dyn PluginCommand<Plugin = Self>>> {
        let mut commands: Vec<Box<dyn PluginCommand<Plugin = Self>>> = vec![Box::new(PolarsCmd)];

        commands.append(&mut aggregation_commands());
        commands.append(&mut boolean_commands());
        commands.append(&mut core_commands());
        commands.append(&mut computation_commands());
        commands.append(&mut data_commands());
        commands.append(&mut datetime_commands());
        commands.append(&mut index_commands());
        commands.append(&mut integer_commands());
        commands.append(&mut selector_commands());
        commands.append(&mut string_commands());
        commands.append(&mut list_commands());
        commands.push(Box::new(scan::Open));
        commands.push(Box::new(call::Call));

        commands.append(&mut cache_commands());
        commands
    }

    fn custom_value_dropped(
        &self,
        _engine: &EngineInterface,
        custom_value: Box<dyn CustomValue>,
    ) -> Result<(), LabeledError> {
        debug!("custom_value_dropped called {custom_value:?}");
        let id = CustomValueType::try_from_custom_value(custom_value, Span::unknown())?.id();
        let _ = self.cache.remove(&id, false);
        Ok(())
    }

    fn custom_value_to_base_value(
        &self,
        engine: &EngineInterface,
        custom_value: Spanned<Box<dyn CustomValue>>,
    ) -> Result<Value, LabeledError> {
        let span = custom_value.span;
        let result = match CustomValueType::try_from_custom_value(custom_value.item, span)? {
            CustomValueType::NuDataFrame(cv) => cv.custom_value_to_base_value(self, engine, span),
            CustomValueType::NuLazyFrame(cv) => cv.custom_value_to_base_value(self, engine, span),
            CustomValueType::NuExpression(cv) => cv.custom_value_to_base_value(self, engine, span),
            CustomValueType::NuLazyGroupBy(cv) => cv.custom_value_to_base_value(self, engine, span),
            CustomValueType::NuWhen(cv) => cv.custom_value_to_base_value(self, engine, span),
            CustomValueType::NuDataType(cv) => cv.custom_value_to_base_value(self, engine, span),
            CustomValueType::NuSchema(cv) => cv.custom_value_to_base_value(self, engine, span),
            CustomValueType::NuSelector(cv) => cv.custom_value_to_base_value(self, engine, span),
        };
        Ok(result?)
    }

    fn custom_value_operation(
        &self,
        engine: &EngineInterface,
        left: Spanned<Box<dyn CustomValue>>,
        operator: Spanned<Operator>,
        right: Value,
    ) -> Result<Value, LabeledError> {
        let left_span = left.span;
        let result = match CustomValueType::try_from_custom_value(left.item, left_span)? {
            CustomValueType::NuDataFrame(cv) => {
                cv.custom_value_operation(self, engine, left_span, operator, right)
            }
            CustomValueType::NuLazyFrame(cv) => {
                cv.custom_value_operation(self, engine, left_span, operator, right)
            }
            CustomValueType::NuExpression(cv) => {
                cv.custom_value_operation(self, engine, left_span, operator, right)
            }
            CustomValueType::NuLazyGroupBy(cv) => {
                cv.custom_value_operation(self, engine, left_span, operator, right)
            }
            CustomValueType::NuWhen(cv) => {
                cv.custom_value_operation(self, engine, left_span, operator, right)
            }
            CustomValueType::NuDataType(cv) => {
                cv.custom_value_operation(self, engine, left_span, operator, right)
            }
            CustomValueType::NuSchema(cv) => {
                cv.custom_value_operation(self, engine, left_span, operator, right)
            }
            CustomValueType::NuSelector(cv) => {
                cv.custom_value_operation(self, engine, left_span, operator, right)
            }
        };
        Ok(result?)
    }

    fn custom_value_follow_path_int(
        &self,
        engine: &EngineInterface,
        custom_value: Spanned<Box<dyn CustomValue>>,
        index: Spanned<usize>,
        // TODO: check if we should respect these
        _optional: bool,
    ) -> Result<Value, LabeledError> {
        let cv_span = custom_value.span;
        let result = match CustomValueType::try_from_custom_value(custom_value.item, cv_span)? {
            CustomValueType::NuDataFrame(cv) => {
                cv.custom_value_follow_path_int(self, engine, cv_span, index)
            }
            CustomValueType::NuLazyFrame(cv) => {
                cv.custom_value_follow_path_int(self, engine, cv_span, index)
            }
            CustomValueType::NuExpression(cv) => {
                cv.custom_value_follow_path_int(self, engine, cv_span, index)
            }
            CustomValueType::NuLazyGroupBy(cv) => {
                cv.custom_value_follow_path_int(self, engine, cv_span, index)
            }
            CustomValueType::NuWhen(cv) => {
                cv.custom_value_follow_path_int(self, engine, cv_span, index)
            }
            CustomValueType::NuDataType(cv) => {
                cv.custom_value_follow_path_int(self, engine, cv_span, index)
            }
            CustomValueType::NuSchema(cv) => {
                cv.custom_value_follow_path_int(self, engine, cv_span, index)
            }
            CustomValueType::NuSelector(cv) => {
                cv.custom_value_follow_path_int(self, engine, cv_span, index)
            }
        };
        Ok(result?)
    }

    fn custom_value_follow_path_string(
        &self,
        engine: &EngineInterface,
        custom_value: Spanned<Box<dyn CustomValue>>,
        column_name: Spanned<String>,
        // TODO: check if we should respect these
        _optional: bool,
        _casing: Casing,
    ) -> Result<Value, LabeledError> {
        let cv_span = custom_value.span;
        let result = match CustomValueType::try_from_custom_value(custom_value.item, cv_span)? {
            CustomValueType::NuDataFrame(cv) => {
                cv.custom_value_follow_path_string(self, engine, cv_span, column_name)
            }
            CustomValueType::NuLazyFrame(cv) => {
                cv.custom_value_follow_path_string(self, engine, cv_span, column_name)
            }
            CustomValueType::NuExpression(cv) => {
                cv.custom_value_follow_path_string(self, engine, cv_span, column_name)
            }
            CustomValueType::NuLazyGroupBy(cv) => {
                cv.custom_value_follow_path_string(self, engine, cv_span, column_name)
            }
            CustomValueType::NuWhen(cv) => {
                cv.custom_value_follow_path_string(self, engine, cv_span, column_name)
            }
            CustomValueType::NuDataType(cv) => {
                cv.custom_value_follow_path_string(self, engine, cv_span, column_name)
            }
            CustomValueType::NuSchema(cv) => {
                cv.custom_value_follow_path_string(self, engine, cv_span, column_name)
            }
            CustomValueType::NuSelector(cv) => {
                cv.custom_value_follow_path_string(self, engine, cv_span, column_name)
            }
        };
        Ok(result?)
    }

    fn custom_value_partial_cmp(
        &self,
        engine: &EngineInterface,
        custom_value: Box<dyn CustomValue>,
        other_value: Value,
    ) -> Result<Option<Ordering>, LabeledError> {
        let result = match CustomValueType::try_from_custom_value(custom_value, other_value.span())?
        {
            CustomValueType::NuDataFrame(cv) => {
                cv.custom_value_partial_cmp(self, engine, other_value)
            }
            CustomValueType::NuLazyFrame(cv) => {
                cv.custom_value_partial_cmp(self, engine, other_value)
            }
            CustomValueType::NuExpression(cv) => {
                cv.custom_value_partial_cmp(self, engine, other_value)
            }
            CustomValueType::NuLazyGroupBy(cv) => {
                cv.custom_value_partial_cmp(self, engine, other_value)
            }
            CustomValueType::NuWhen(cv) => cv.custom_value_partial_cmp(self, engine, other_value),
            CustomValueType::NuDataType(cv) => {
                cv.custom_value_partial_cmp(self, engine, other_value)
            }
            CustomValueType::NuSchema(cv) => cv.custom_value_partial_cmp(self, engine, other_value),
            CustomValueType::NuSelector(cv) => {
                cv.custom_value_partial_cmp(self, engine, other_value)
            }
        };
        Ok(result?)
    }
}

/// Serves the plugin over stdio with the built-in scan sources plus `extra`, and returns when
/// the engine closes the connection.
///
/// Each element of `extra` is one crate's `scan_sources()`. Every binary is this call and
/// nothing else: the published `nu_plugin_polars_dyn` passes `&[]`, and the `main.rs` that
/// `nu-polars-dyn-build` generates passes one entry per crate it was given. A name or a suffix
/// registered twice is printed to stderr and exits the process with 1.
///
/// Must be called from `main` before any thread is spawned: it sets `POLARS_ALLOW_EXTENSION`,
/// which polars needs for aggregates over object dtypes.
pub fn serve(extra: &[&'static [&'static dyn ScanSource]]) {
    env_logger::init();

    // SAFETY: the process is still single-threaded — `serve_plugin` below spawns the first
    // thread — so no other thread can read the environment while it is being written.
    unsafe {
        std::env::set_var("POLARS_ALLOW_EXTENSION", "true");
    }

    let sources: Vec<&'static dyn ScanSource> = scan::builtin::BUILTIN
        .iter()
        .copied()
        .chain(extra.iter().flat_map(|sources| sources.iter().copied()))
        .collect();

    match PolarsPlugin::new(Box::leak(sources.into_boxed_slice())) {
        Ok(ref plugin) => serve_plugin(plugin, MsgPackSerializer {}),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

#[allow(dead_code)]
pub(crate) fn handle_panic<F, R>(f: F, span: Span) -> Result<R, ShellError>
where
    F: FnOnce() -> Result<R, ShellError>,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(inner_result) => inner_result,
        Err(_) => Err(ShellError::Generic(GenericError::new(
            "Panic occurred",
            "",
            span,
        ))),
    }
}

/// Runs `f` and turns a panic into a `ShellError` whose detail is the panic message, prefixed
/// with `what`. polars-stream reports nodes it has not implemented with `todo!` instead of a
/// `PolarsError`, so the message is the only place the user learns what was refused.
pub(crate) fn handle_panic_with_message<F, R>(f: F, what: &str, span: Span) -> Result<R, ShellError>
where
    F: FnOnce() -> Result<R, ShellError>,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(inner_result) => inner_result,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            Err(ShellError::Generic(GenericError::new(
                "Panic occurred",
                format!("{what}: {message}"),
                span,
            )))
        }
    }
}

#[cfg(test)]
pub mod test {
    use super::*;
    use crate::values::PolarsPluginObject;
    use nu_plugin_test_support::PluginTest;
    use nu_protocol::{ShellError, Span, engine::Command};

    impl PolarsPlugin {
        /// Creates a new polars plugin in test mode
        pub fn new_test_mode() -> Result<Self, ShellError> {
            PolarsPlugin::new(scan::builtin::BUILTIN)
        }
    }

    pub fn test_polars_plugin_command(command: &impl PluginCommand) -> Result<(), ShellError> {
        test_polars_plugin_command_with_decls(command, vec![])
    }

    pub fn test_polars_plugin_command_with_decls(
        command: &impl PluginCommand,
        decls: Vec<Box<dyn Command>>,
    ) -> Result<(), ShellError> {
        let plugin = PolarsPlugin::new_test_mode()?;
        let examples = command.examples();

        // we need to cache values in the examples
        for example in &examples {
            if let Some(ref result) = example.result {
                // if it's a polars plugin object, try to cache it
                if let Ok(obj) = PolarsPluginObject::try_from_value(&plugin, result) {
                    let id = obj.id();
                    plugin.cache.insert(id, obj, Span::test_data()).unwrap();
                }
            }
        }

        let mut plugin_test = PluginTest::new(command.name(), plugin.into())?;

        for decl in decls {
            let _ = plugin_test.add_decl(decl)?;
        }
        plugin_test.test_examples(&examples)
    }
}
