//! Sources that `polars_dyn open` can read.
//!
//! A source string names a chain of [`ScanSource`]s: the scheme picks the first, which opens the
//! URL into a [`ReadAt`]; every suffix stripped off the end picks one more, which either wraps
//! the bytes into other bytes or reads them into a `LazyFrame`. The [`ScanRegistry`] holds the
//! sources the bin registers at plugin construction — the built-ins in [`builtin`] plus whatever
//! the bin compiled in — and resolves a source string into a [`Chain`]. No source knows what
//! comes before or after it.

pub mod builtin;
mod chunked;
mod command;
mod opts;
mod read_at;

pub use command::Open;
pub use opts::{no_opts, overlay_opts, parse_opts};
pub use read_at::{ReadAt, ReadAtCursor, read_fully};

use std::sync::Arc;

use nu_protocol::{ShellError, Span, shell_error::generic::GenericError};
use polars::prelude::{LazyFrame, PolarsResult, polars_bail};

/// One step of a chain: a way to open a URL, to wrap bytes into other bytes, or to read bytes
/// into a `LazyFrame`.
///
/// A source implements the steps it performs and leaves the others at their defaults, which are
/// errors. What string named the bytes and where they come from are the plugin's business, so a
/// source reads a local file, a remote object and the decompressed form of either alike. The
/// options are bytes whose meaning is the implementation's own contract; the plugin passes them
/// through untouched.
pub trait ScanSource: Send + Sync {
    /// The registered name, matched against `--format` and the keys of `--opts`.
    fn name(&self) -> &'static str;

    /// Suffixes stripped off the end of the source string, longest match first across all
    /// sources, each putting this source next in the chain. Each must start with `.`, e.g.
    /// `[".seek.zst"]`. Empty for a source that only opens a scheme.
    fn suffixes(&self) -> &'static [&'static str];

    /// The URL schemes this source opens as the first of a chain, e.g. `["ssh"]`. Empty when it
    /// opens none.
    fn schemes(&self) -> &'static [&'static str] {
        &[]
    }

    /// Opens `url` — the whole source string, scheme included — into bytes. Called when this
    /// source was chosen by its scheme. `opts` is this source's entry of `--opts` encoded as
    /// JSON, or empty when there was none.
    fn open(&self, url: &str, opts: &[u8]) -> PolarsResult<Arc<dyn ReadAt>> {
        let _ = (url, opts);
        polars_bail!(ComputeError: "`{}` does not open a URL", self.name())
    }

    /// Turns `source` into other bytes, such as its decompressed form. Called when this source
    /// is in the middle of a chain.
    fn wrap(&self, source: Arc<dyn ReadAt>, opts: &[u8]) -> PolarsResult<Arc<dyn ReadAt>> {
        let _ = (source, opts);
        polars_bail!(
            ComputeError:
            "`{}` does not wrap bytes; it can only end a chain", self.name()
        )
    }

    /// Builds a `LazyFrame` over `source` without collecting it. Called when this source ends a
    /// chain.
    fn scan(&self, source: Arc<dyn ReadAt>, opts: &[u8]) -> PolarsResult<LazyFrame> {
        let _ = (source, opts);
        polars_bail!(
            ComputeError:
            "`{}` does not read bytes into a frame; put a format after it, as in \
             `--format {},ndjson`", self.name(), self.name()
        )
    }
}

/// The sources a source string resolved to, in the order they are called.
pub struct Chain {
    /// The source chosen by the scheme, which opens the URL.
    pub head: &'static dyn ScanSource,
    /// The sources chosen by the suffixes or by `--format` before the last, which wrap.
    pub middle: Vec<&'static dyn ScanSource>,
    /// The last source chosen, which scans.
    pub last: &'static dyn ScanSource,
}

impl Chain {
    /// The names in call order, head first.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        std::iter::once(self.head.name())
            .chain(self.middle.iter().map(|s| s.name()))
            .chain(std::iter::once(self.last.name()))
    }
}

/// The built-ins and the sources registered with the plugin, with names, schemes and suffixes
/// known to be unique.
pub struct ScanRegistry {
    sources: &'static [&'static dyn ScanSource],
}

impl ScanRegistry {
    /// The built-ins plus `sources`. Fails when two entries share a name, a scheme or a suffix.
    ///
    /// ```
    /// # fn main() -> Result<(), nu_protocol::ShellError> {
    /// use nu_plugin_polars::scan::ScanRegistry;
    /// use nu_protocol::Span;
    /// let registry = ScanRegistry::new(&[])?;
    /// let span = Span::unknown();
    /// let chain = registry.resolve("data.parquet", None, span)?;
    /// assert_eq!(chain.names().collect::<Vec<_>>(), ["file", "parquet"]);
    /// let chain = registry.resolve("file:///tmp/data.txt", Some("csv"), span)?;
    /// assert_eq!(chain.names().collect::<Vec<_>>(), ["file", "csv"]);
    /// assert!(registry.resolve("data.xyz", None, span).is_err());
    /// assert!(registry.resolve("ftp://h/data.csv", None, span).is_err());
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(sources: &'static [&'static dyn ScanSource]) -> Result<Self, ShellError> {
        let registry = Self { sources };
        let entries: Vec<&dyn ScanSource> = registry.entries().collect();
        for (i, a) in entries.iter().enumerate() {
            for b in &entries[i + 1..] {
                if a.name() == b.name() {
                    return Err(registry_error(format!(
                        "scan source name `{}` is registered twice",
                        a.name()
                    )));
                }
                if let Some(scheme) = a.schemes().iter().find(|s| b.schemes().contains(s)) {
                    return Err(registry_error(format!(
                        "scan sources `{}` and `{}` both claim the scheme `{scheme}`",
                        a.name(),
                        b.name()
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

    fn entries(&self) -> impl Iterator<Item = &'static dyn ScanSource> + '_ {
        builtin::BUILTIN
            .iter()
            .copied()
            .chain(self.sources.iter().copied())
    }

    /// The chain `source` names: the scheme's source first (`file` when there is no scheme), then
    /// one source per suffix stripped off the end, or the names of `format` (comma-separated)
    /// instead of the suffixes.
    ///
    /// Fails, listing what is registered and pointing at `span`, when the scheme has no source,
    /// when `format` names an unknown source, or when no suffix matches.
    pub fn resolve(
        &self,
        source: &str,
        format: Option<&str>,
        span: Span,
    ) -> Result<Chain, ShellError> {
        let head = match url_scheme(source) {
            Some(scheme) => self
                .entries()
                .find(|s| s.schemes().contains(&scheme))
                .ok_or_else(|| {
                    no_source_error(
                        format!("No scan source for scheme `{scheme}`"),
                        format!("registered schemes: {}", self.schemes().join(", ")),
                        span,
                    )
                })?,
            None => builtin::FILE,
        };
        let mut rest = match format {
            Some(names) => names
                .split(',')
                .map(|name| {
                    self.find_by_name(name.trim()).ok_or_else(|| {
                        no_source_error(
                            format!("No scan source named `{name}`"),
                            format!("registered: {}", self.names().join(", ")),
                            span,
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            None => self.strip_suffixes(source),
        };
        let Some(last) = rest.pop() else {
            return Err(no_source_error(
                format!("No scan source for `{source}`"),
                format!("registered: {}", self.suffixed_names().join(", ")),
                span,
            ));
        };
        Ok(Chain {
            head,
            middle: rest,
            last,
        })
    }

    /// The sources whose suffixes end `source`, stripped one at a time from the end — longest
    /// match first — until none matches.
    fn strip_suffixes(&self, source: &str) -> Vec<&'static dyn ScanSource> {
        let mut rest = Vec::new();
        let mut remaining = source;
        while let Some((suffix, found)) = self.longest_suffix(remaining) {
            rest.push(found);
            remaining = &remaining[..remaining.len() - suffix.len()];
        }
        rest
    }

    fn longest_suffix(&self, source: &str) -> Option<(&'static str, &'static dyn ScanSource)> {
        self.entries()
            .flat_map(|s| s.suffixes().iter().map(move |suffix| (*suffix, s)))
            .filter(|(suffix, _)| source.ends_with(suffix))
            .max_by_key(|(suffix, _)| suffix.len())
    }

    fn find_by_name(&self, name: &str) -> Option<&'static dyn ScanSource> {
        self.entries().find(|s| s.name() == name)
    }

    /// Every registered name, in registration order.
    fn names(&self) -> Vec<&'static str> {
        self.entries().map(|s| s.name()).collect()
    }

    /// The names of the sources a suffix can choose.
    fn suffixed_names(&self) -> Vec<&'static str> {
        self.entries()
            .filter(|s| !s.suffixes().is_empty())
            .map(|s| s.name())
            .collect()
    }

    fn schemes(&self) -> Vec<&'static str> {
        self.entries()
            .flat_map(|s| s.schemes().iter().copied())
            .collect()
    }
}

fn no_source_error(title: String, detail: String, span: Span) -> ShellError {
    ShellError::Generic(
        GenericError::new(title, detail, span)
            .with_help("Pass --format with the registered names, comma-separated"),
    )
}

/// The `<scheme>` of `<scheme>://...` per RFC 3986 — a letter, then letters, digits, `+`, `-` or
/// `.` — or `None` when `source` has none.
pub fn url_scheme(source: &str) -> Option<&str> {
    let (scheme, _) = source.split_once("://")?;
    let mut chars = scheme.chars();
    let well_formed = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    well_formed.then_some(scheme)
}

fn registry_error(msg: String) -> ShellError {
    ShellError::Generic(GenericError::new_internal(msg, ""))
}
