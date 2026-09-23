//! Sources that `polars_dyn open` can read.
//!
//! A [`ScanSource`] turns a handle over the bytes and an options blob into a `LazyFrame`. The
//! [`ScanRegistry`] holds the sources the bin registers at plugin construction and picks one by
//! name or by the longest matching suffix. The built-in sources — parquet, csv, ipc and ndjson as
//! polars reads them — are in [`builtin`] and take the path or URL instead, since polars opens
//! those itself. Anything else is compiled in by the bin.

pub mod builtin;
mod command;
mod opts;
mod read_at;

pub use command::Open;
pub use opts::{overlay_opts, parse_opts};
pub use read_at::ReadAt;

use std::sync::Arc;

use nu_protocol::{ShellError, shell_error::generic::GenericError};
use polars::prelude::{LazyFrame, PolarsResult};

use builtin::Builtin;

/// A way to read one family of sources into a `LazyFrame`.
///
/// The source arrives as bytes to read by offset; what string named it and how it was opened
/// are the plugin's business, so a source reads a local file and a remote object alike. The
/// options are bytes whose meaning is the implementation's own contract; the plugin passes them
/// through untouched.
pub trait ScanSource: Send + Sync {
    /// The registered name, matched against `--format`.
    fn name(&self) -> &'static str;

    /// Suffixes matched against the end of the source string when `--format` is absent, longest
    /// match first across all sources. Each must start with `.`, e.g. `[".logfmt", ".logfmt.seek.zst"]`.
    fn suffixes(&self) -> &'static [&'static str];

    /// Builds a `LazyFrame` over `source` without collecting it. `opts` is the `--opts` record
    /// encoded as JSON, or empty when the flag was omitted.
    fn scan(&self, source: Arc<dyn ReadAt>, opts: &[u8]) -> PolarsResult<LazyFrame>;
}

/// One entry of the registry: a built-in, which polars reads from the path or URL, or a
/// [`ScanSource`] the bin compiled in.
#[derive(Clone, Copy)]
pub enum Registered {
    Builtin(&'static Builtin),
    Source(&'static dyn ScanSource),
}

impl Registered {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Builtin(builtin) => builtin.name,
            Self::Source(source) => source.name(),
        }
    }

    pub fn suffixes(&self) -> &'static [&'static str] {
        match self {
            Self::Builtin(builtin) => builtin.suffixes,
            Self::Source(source) => source.suffixes(),
        }
    }
}

/// The built-ins and the sources registered with the plugin, with names and suffixes known to be
/// unique.
pub struct ScanRegistry {
    sources: &'static [&'static dyn ScanSource],
}

impl ScanRegistry {
    /// The built-ins plus `sources`. Fails when two entries share a name or a suffix.
    ///
    /// ```
    /// # fn main() -> Result<(), nu_protocol::ShellError> {
    /// use nu_plugin_polars::scan::ScanRegistry;
    /// let registry = ScanRegistry::new(&[])?;
    /// assert_eq!(registry.find_by_suffix("data.parquet").map(|s| s.name()), Some("parquet"));
    /// assert_eq!(registry.find_by_name("csv").map(|s| s.name()), Some("csv"));
    /// assert!(registry.find_by_suffix("data.xyz").is_none());
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(sources: &'static [&'static dyn ScanSource]) -> Result<Self, ShellError> {
        let registry = Self { sources };
        let entries: Vec<Registered> = registry.entries().collect();
        for (i, a) in entries.iter().enumerate() {
            for b in &entries[i + 1..] {
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
        Ok(registry)
    }

    fn entries(&self) -> impl Iterator<Item = Registered> + '_ {
        builtin::BUILTIN
            .iter()
            .map(Registered::Builtin)
            .chain(self.sources.iter().map(|s| Registered::Source(*s)))
    }

    pub fn find_by_name(&self, name: &str) -> Option<Registered> {
        self.entries().find(|s| s.name() == name)
    }

    /// The entry whose longest suffix ends `source`.
    pub fn find_by_suffix(&self, source: &str) -> Option<Registered> {
        self.entries()
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
        self.entries().map(|s| s.name())
    }
}

fn registry_error(msg: String) -> ShellError {
    ShellError::Generic(GenericError::new_internal(msg, ""))
}
