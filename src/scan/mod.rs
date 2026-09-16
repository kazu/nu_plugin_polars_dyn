//! Sources that `polars_dyn open` can read.
//!
//! A [`ScanSource`] turns a source string and an options blob into a `LazyFrame`. The
//! [`ScanRegistry`] holds the sources the bin registers at plugin construction and picks one by
//! name or by the longest matching suffix. The built-in parquet / csv / ipc / ndjson sources are
//! in [`builtin`].

pub mod builtin;
mod command;
mod opts;

pub use command::Open;
pub use opts::overlay_opts;

use nu_protocol::{ShellError, shell_error::generic::GenericError};
use polars::prelude::{LazyFrame, PolarsResult};

/// A way to read one family of sources into a `LazyFrame`.
///
/// The boundary takes only values that can later cross an FFI boundary: the source as a string
/// and the options as bytes. The meaning of both is the implementation's own contract; the
/// plugin passes them through untouched.
pub trait ScanSource: Send + Sync {
    /// The registered name, matched against `--format`.
    fn name(&self) -> &'static str;

    /// Suffixes matched against the end of the source string when `--format` is absent, longest
    /// match first across all sources. Each must start with `.`, e.g. `[".logfmt", ".logfmt.zst"]`.
    fn suffixes(&self) -> &'static [&'static str];

    /// Builds a `LazyFrame` over `source` without collecting it. `opts` is the `--opts` record
    /// encoded as JSON, or empty when the flag was omitted.
    fn scan(&self, source: &str, opts: &[u8]) -> PolarsResult<LazyFrame>;
}

/// The sources registered with the plugin, with names and suffixes known to be unique.
pub struct ScanRegistry {
    sources: &'static [&'static dyn ScanSource],
}

impl ScanRegistry {
    /// Fails when two sources share a name or a suffix.
    ///
    /// ```
    /// # fn main() -> Result<(), nu_protocol::ShellError> {
    /// use nu_plugin_polars::scan::{ScanRegistry, builtin::BUILTIN};
    /// let registry = ScanRegistry::new(BUILTIN)?;
    /// assert_eq!(registry.find_by_suffix("data.parquet").map(|s| s.name()), Some("parquet"));
    /// assert_eq!(registry.find_by_name("csv").map(|s| s.name()), Some("csv"));
    /// assert!(registry.find_by_suffix("data.xyz").is_none());
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(sources: &'static [&'static dyn ScanSource]) -> Result<Self, ShellError> {
        for (i, a) in sources.iter().enumerate() {
            for b in &sources[i + 1..] {
                if a.name() == b.name() {
                    return Err(registry_error(format!(
                        "scan source name `{}` is registered twice",
                        a.name()
                    )));
                }
                if let Some(suffix) = a.suffixes().iter().find(|s| b.suffixes().contains(s)) {
                    return Err(registry_error(format!(
                        "scan sources `{}` and `{}` both claim the suffix `{suffix}`",
                        a.name(),
                        b.name()
                    )));
                }
            }
        }
        Ok(Self { sources })
    }

    pub fn find_by_name(&self, name: &str) -> Option<&'static dyn ScanSource> {
        self.sources.iter().copied().find(|s| s.name() == name)
    }

    /// The source whose longest suffix ends `source`.
    pub fn find_by_suffix(&self, source: &str) -> Option<&'static dyn ScanSource> {
        self.sources
            .iter()
            .copied()
            .filter_map(|s| {
                s.suffixes()
                    .iter()
                    .filter(|suffix| source.ends_with(*suffix))
                    .map(|suffix| suffix.len())
                    .max()
                    .map(|len| (len, s))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, s)| s)
    }

    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.sources.iter().map(|s| s.name())
    }
}

fn registry_error(msg: String) -> ShellError {
    ShellError::Generic(GenericError::new_internal(msg, ""))
}
