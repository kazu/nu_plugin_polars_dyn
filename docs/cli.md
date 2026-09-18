# Commands

The commands this fork adds or replaces, and their options. The rest are upstream
`nu_plugin_polars` as they were, so `help polars_dyn` lists them and `help polars_dyn <command>`
explains one.

- [`polars_dyn open`](#polars_dyn-open)
- [`polars_dyn collect --streaming`](#polars_dyn-collect---streaming)
- [`polars_dyn call`](#polars_dyn-call)
- [`polars_dyn store-ls` / `store-rm`](#polars_dyn-store-ls--store-rm)
- [`nu-polars-dyn-build`](#nu-polars-dyn-build)

## `polars_dyn open`

```
polars_dyn open <source> [--format (-f) <name>] [--opts (-o) <record>]  → LazyFrame
```

`source` is handed to the scan source as it stands, uninterpreted: a local path, a URL such as
`s3://...`, or whatever else that scan source reads. Nothing is collected, so what comes back is a
`LazyFrame` that has not read anything yet.

### `--format`

Picks the format by its registered name. Without it, the format is the **longest match** between
the end of `source` and the registered suffixes (`app.logfmt.seek.zst` goes to the
`.logfmt.seek.zst` source, not to `.seek.zst`). If nothing matches, the error lists the registered
names.

The built-ins are the four polars reads itself:

| name | suffixes | top level of `--opts` |
|---|---|---|
| `parquet` | `.parquet` / `.parq` / `.pq` | `ParquetOptions` |
| `csv` | `.csv` | `CsvReadOptions` |
| `ipc` | `.arrow` / `.ipc` | `IpcScanOptions` |
| `ndjson` | `.ndjson` / `.jsonl` | `NDJsonReadOptions` |

### `--opts`

The format's own options, as a record. What it holds is up to each scan source; the plugin turns it
into JSON and passes it on without looking inside. Omitting it passes nothing.

For the built-ins the top level is the struct in the table above, and the `scan` key is polars'
`UnifiedScanArgs` (`glob`, `cloud_options`, `hive_options`, `row_index`, `pre_slice` and so on).
Both are polars' own serde forms, so a `u8` such as the csv separator is written as a number rather
than a character.

```nu
polars_dyn open data.csv --opts {has_header: false}
polars_dyn open data.csv --opts {parse_options: {separator: 59}, scan: {glob: false}}
polars_dyn open data.ndjson --opts {infer_schema_length: 1000}
```

A misspelled key is an error: polars' serde rejects fields it does not know.

## `polars_dyn collect --streaming`

With `--streaming`, the collect runs on polars' streaming engine instead of the in-memory one. The
default is in-memory, and the implicit collects — handing a lazy frame to `into-nu`, and the like —
stay in-memory either way.

```nu
polars_dyn open data.parquet | polars_dyn filter ((polars_dyn col a) > 1) | polars_dyn collect --streaming
```

A scan source of your own (usually an `AnonymousScan`) cannot be collected on the streaming engine.
It fails with `collecting on the streaming engine: not yet implemented: unimplemented:
AnonymousScan` rather than quietly falling back to in-memory. The four built-in formats are not
affected.

## `polars_dyn call`

Calls a function exported by a polars expression plugin (`.so` / `.dll`) as an expression. It uses
polars' own expression plugin mechanism (`_polars_plugin_<symbol>`), so a plugin written for Python
can be called as it is.

```
polars_dyn call <lib> <symbol> [args...] [--kwargs (-k) <record>] [options]  → Expr
```

- `lib` is the path to the shared library. A relative path is resolved **when the command runs**,
  against the current directory, not at collect time.
- `symbol` is the function name: the `<symbol>` part of the `_polars_plugin_<symbol>` the plugin
  exports.
- `args` are the input expressions. A string is taken as a column name.
- `--kwargs` are keyword arguments, passed to the plugin as pickle bytes.

| option | meaning |
|---|---|
| `--elementwise` | the function maps each row independently of the others |
| `--returns-scalar` | the function returns one value per group |
| `--changes-length` | the output length may differ from the input length |
| `--cast-to-supertype` | cast all inputs to their common supertype before calling |
| `--pass-name` | pass the input column name to the function (for `over` and `group-by`) |

```nu
[[a]; [1] [2]] | polars_dyn into-df | polars_dyn select (polars_dyn call ./libmy_plugin.so add a --kwargs {n: 10} --elementwise) | polars_dyn collect
polars_dyn call plugins/libmy_plugin.so distance (polars_dyn col x) (polars_dyn col y)
```

## `polars_dyn store-ls` / `store-rm`

Lists the polars objects the plugin holds, and removes one explicitly. What nushell holds is a
handle; the object itself stays in the plugin process.

```nu
polars_dyn store-ls
polars_dyn store-ls | get key | first | polars_dyn store-rm $in
```

`store-rm` takes the keys as arguments; there is no `--all`.

## `nu-polars-dyn-build`

The builder that produces a plugin binary with scan source crates compiled in. It is a command of
its own rather than a plugin, and `cargo install` puts it next to the plugin.

```
nu-polars-dyn-build <crate>... [--path <name>=<dir>]... [--git <name>=<url>]...
                    [--out <dir>] [--debug]
```

How to use it, and how to write the crate, is in [custom_build.md](./custom_build.md).
