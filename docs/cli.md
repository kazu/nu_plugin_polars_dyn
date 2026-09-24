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
polars_dyn open <source> [--format (-f) <a,b,c>] [--opts (-o) <record>]  → LazyFrame
```

`source` names a **chain** of scan sources: the URL scheme picks the first, which opens the bytes,
and every suffix at the end of the string picks one more, which either turns the bytes into other
bytes or reads them into a frame. `ssh://host/log/events.jsonl.seek.zst` is `ssh`, `seek-zst`,
`ndjson`: sftp opens the file, `seek-zst` decompresses it, `ndjson` reads the lines. A path without
a scheme is opened by `file`. No source in the chain knows what comes before or after it, so any
opener works with any decompressor and any format.

Nothing is collected, so what comes back is a `LazyFrame` that has not read the rows yet.

### The chain

1. `<scheme>://` picks the source that registered that scheme; without a scheme it is `file`. A
   scheme nobody registered is an error listing the registered schemes.
2. The **longest** registered suffix ending the string is stripped and its source appended; this
   repeats until no suffix matches (`x.jsonl.seek.zst` strips `.seek.zst`, then `.jsonl`). If
   nothing was stripped, the error lists the sources a suffix can pick.
3. A path without a scheme is made absolute against the current directory, and if that has a glob
   metacharacter (`*`, `?`, `[`), in the current directory too, it is expanded on the local file
   system; the suffixes are those of the pattern (`data/*.jsonl` is `file`, `ndjson`). As in
   upstream, directories and empty files are skipped and the matches are sorted as strings. A
   glob without a match is an error. A URL with a scheme is never expanded.
4. For each file, the first source opens the URL, every one in between wraps the bytes, the last
   reads them into the frame. A last source that only wraps (`./x.seek.zst` alone) is an error
   naming it. The frames of a glob are stacked in that order; files whose columns differ are an
   error when collected, not aligned. Unlike upstream, several parquet or ipc files are not read in
   parallel as one scan, and hive partitions are not read.

### `--format`

Replaces step 2 with the names given, comma-separated and in chain order; the scheme still comes
from the string. `--format csv` reads a `.txt` as csv, `--format seek-zst,ndjson` reads a file
whose name says nothing. A name nobody registered is an error listing the registered names.

The sources every binary has:

| name | picked by | `--opts` under its name |
|---|---|---|
| `file` | no scheme, or `file://` | none |
| `parquet` | `.parquet` / `.parq` / `.pq` | `ParquetOptions`, plus `scan` |
| `csv` | `.csv` | `CsvReadOptions` |
| `ipc` | `.arrow` / `.ipc` | `IpcScanOptions`, plus `scan` |
| `ndjson` | `.ndjson` / `.jsonl` | `NDJsonReadOptions` |

Anything else — `seek-zst`, `ssh`, `logfmt` — is a crate compiled in with
[`nu-polars-dyn-build`](./custom_build.md).

### `--opts`

A record keyed by scan source name, each value the record that source takes. The plugin turns each
value into JSON and passes it to its source without looking inside; a key naming no source in the
chain is an error, and a source that was given nothing gets an empty record.

```nu
polars_dyn open data.csv --opts {csv: {has_header: false}}
polars_dyn open data.csv --opts {csv: {parse_options: {separator: 59}}}
polars_dyn open data.ndjson --opts {ndjson: {infer_schema_length: 1000}}
polars_dyn open data.parquet --opts {parquet: {scan: {row_index: {name: i, offset: 0}}}}
polars_dyn open ssh://host/var/log/events.jsonl.seek.zst --opts {ssh: {port: 2222}, ndjson: {ignore_errors: true}}
```

For the built-in formats the value is polars' own option struct in its serde form, so a `u8` such
as the csv separator is a number rather than a character, and a misspelled key is an error.

parquet and ipc are read by polars' own scan over the bytes, so their `scan` key is polars'
`UnifiedScanArgs` (`row_index`, `pre_slice`, `cloud_options` and so on) as before.

csv and ndjson are read by the plugin: the bytes are cut into chunks of `chunk_size` bytes, each cut
moved forward to the next newline, and the chunks are parsed in parallel with polars' reader and
concatenated. The schema is settled on the first chunk, and `infer_schema_length` looks no further
than it. The cut does not know csv quoting: a newline inside a quoted field can be a cut, which is
a parse error, so a csv with such fields is read with a `chunk_size` of at least its length. Every chunk is read by a reader holding the same options, so the csv options whose subject
is the whole file are refused rather than applied once per chunk: `n_rows`, `skip_rows`,
`skip_lines`, `skip_rows_after_header` (use `polars_dyn slice`), `columns`, `projection`
(`polars_dyn select`), `column_names_overwrite` (`polars_dyn rename`) and `row_index`, which has
no equivalent. The `scan` key is not taken by these two.

## `polars_dyn collect --streaming`

With `--streaming`, the collect runs on polars' streaming engine instead of the in-memory one. The
default is in-memory, and the implicit collects — handing a lazy frame to `into-nu`, and the like —
stay in-memory either way.

```nu
polars_dyn open data.parquet | polars_dyn filter ((polars_dyn col a) > 1) | polars_dyn collect --streaming
```

Only parquet and ipc can be collected there: they are polars' own scan. csv, ndjson and every scan
source of your own are an `AnonymousScan`, which fails with `collecting on the streaming engine: not
yet implemented: unimplemented: AnonymousScan` rather than quietly falling back to in-memory.

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
handle; the object itself stays in the plugin process. The plugin frees an object once nushell
has dropped every handle to it, so `store-rm` is for the ones still held — by a variable, say, or
by the last value a command printed.

```nu
polars_dyn store-ls
polars_dyn store-ls | get key | first | polars_dyn store-rm $in
```

`store-rm` takes the keys as arguments; there is no `--all`.

## `nu-polars-dyn-build`

The builder that produces a plugin binary with scan source crates compiled in. It is a command of
its own rather than a plugin, and `cargo install` puts it next to the plugin.

```
nu-polars-dyn-build [OPTIONS] <crate>...
```

`nu-polars-dyn-build --help` lists the options. How to use it, and how to write the crate, is in
[custom_build.md](./custom_build.md).