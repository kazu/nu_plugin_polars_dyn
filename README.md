# nu_plugin_polars_dyn

[![CI](https://img.shields.io/github/actions/workflow/status/kazu/nu_plugin_polars_dyn/ci.yml?branch=main)](https://github.com/kazu/nu_plugin_polars_dyn/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)

**A nushell dataframe plugin whose readable formats you can add yourself.**

A fork of nushell's [`nu_plugin_polars`][upstream]. What travels the pipe is a lazy plan:
`polars_dyn open` hands back a `LazyFrame`, each command after it adds to the plan, and nothing is
read until `polars_dyn collect` — a filter or a choice of columns takes effect in the read itself.
No Python anywhere.

| nushell | polars |
| --- | --- |
| 0.114.1 | 0.55.2 |

## Table of contents

- [What this fork changes](#what-this-fork-changes)
- [Install](#install)
- [Usage](#usage)
- [Adding a custom scanner](#adding-a-custom-scanner)
  - [The bundled `.seek.zst`](#the-bundled-seekzst)
- [Repository layout](#repository-layout)
- [Development](#development)
- [License](#license)

## What this fork changes

- **[`polars_dyn open`](docs/cli.md#polars_dyn-open)** — the per-format branching is replaced by a
  registry of scan sources. The format is `--format`, the read options are a record in `--opts`.
- **Custom scanners** — a format other than parquet, csv, ipc and ndjson is a Rust crate you write,
  and [`nu-polars-dyn-build`](docs/cli.md#nu-polars-dyn-build) compiles it in and hands you **your
  own plugin binary**. As many crates as you like go into one binary.
- **[`polars_dyn call`](docs/cli.md#polars_dyn-call)** — calls a function exported by a polars
  expression plugin (a `.so`) as an expression.
- **[`polars_dyn collect --streaming`](docs/cli.md#polars_dyn-collect---streaming)** — collects on
  polars' streaming engine.
- **The `polars_dyn` prefix** — this plugin and the upstream `polars` can be registered in the same
  nushell.

The inherited commands (`filter`, `select`, `group-by`, `sort-by` and the rest) are upstream's.
The ideas are in nushell's book, [Dataframes](https://www.nushell.sh/book/dataframes.html), the
list is under [dataframe commands](https://www.nushell.sh/commands/categories/dataframe.html)
(prefixed `polars` there), and `help polars_dyn` has them at hand.

## Install

Not on crates.io yet, so build it from the repository:

```sh
git clone https://github.com/kazu/nu_plugin_polars_dyn
cd nu_plugin_polars_dyn
cargo install --path . --locked
```

That installs the plugin and the builder `nu-polars-dyn-build`. Register the plugin from nushell:

```nu
plugin add ~/.cargo/bin/nu_plugin_polars_dyn
plugin use polars_dyn
```

## Usage

```nu
polars_dyn open data.csv | polars_dyn collect
polars_dyn open data.csv --opts {has_header: false} | polars_dyn collect
polars_dyn open data.txt --format csv | polars_dyn collect
polars_dyn open data.parquet | polars_dyn filter ((polars_dyn col a) > 1) | polars_dyn collect --streaming
[[a]; [1] [2]] | polars_dyn into-df | polars_dyn select (polars_dyn call ./libmy_plugin.so add a --kwargs {n: 10} --elementwise) | polars_dyn collect
```

The options each command takes are under [`polars_dyn open`](docs/cli.md#polars_dyn-open),
[`polars_dyn collect --streaming`](docs/cli.md#polars_dyn-collect---streaming),
[`polars_dyn call`](docs/cli.md#polars_dyn-call) and
[`polars_dyn store-ls` / `store-rm`](docs/cli.md#polars_dyn-store-ls--store-rm).

## Adding a custom scanner

What `polars_dyn open` reads can be extended. A scan source is a lib crate exporting one function;
the cargo project and the `main.rs` are written by `nu-polars-dyn-build`.

```rust
pub fn scan_sources() -> &'static [&'static dyn nu_plugin_polars::scan::ScanSource];
```

```nu
nu-polars-dyn-build my_scan_source --path my_scan_source=../my_scan_source
plugin add ./nu_plugin_polars_dyn
```

Crates can be listed one after another, so several custom scanners go into one binary:

```nu
nu-polars-dyn-build seekzstdsep_scan my_scan_source --path seekzstdsep_scan=./seekzstdsep-scan --path my_scan_source=../my_scan_source
```

The result replaces the standard binary and keeps the built-in formats. How to write the crate,
every option of the builder, and the limits an `AnonymousScan` carries (no `collect --streaming`, and
a `slice` with an offset does not reach the read) are in
[docs/custom_build.md](docs/custom_build.md).

### The bundled `.seek.zst`

[`seekzstdsep-scan/`](./seekzstdsep-scan/) is a scan source you can build in as it is. It reads the
seekable zstd files [seekzstdsep][] writes: `.csv.seek.zst`, `.ndjson.seek.zst` and
`.jsonl.seek.zst`.

```nu
seekzstdsep compress events.jsonl   # -> events.jsonl.seek.zst
nu-polars-dyn-build seekzstdsep_scan --path seekzstdsep_scan=./seekzstdsep-scan
```

It is there to keep an appended text log as a log and still read it with polars. The disk drops by
the compression ratio and reading costs about what the plain file costs. If the file exists to be
analysed and nothing else, parquet beats it on every axis. What `--opts` takes is in the crate's
module documentation, [`seekzstdsep-scan/src/`](./seekzstdsep-scan/src/).

## Repository layout

- [`src/`](./src/) — the plugin. [`src/scan/`](./src/scan/) is the registry and `polars_dyn open`,
  [`src/call.rs`](./src/call.rs) is `polars_dyn call`,
  [`src/bin/nu-polars-dyn-build.rs`](./src/bin/nu-polars-dyn-build.rs) is the builder.
- [`seekzstdsep-scan/`](./seekzstdsep-scan/) — the `.seek.zst` scan source. Not part of the plugin
  binary.
- [`tests/`](./tests/) — integration tests that spawn `nu`.
  [`tests/expr_plugin`](./tests/expr_plugin/) is an expression plugin for `polars_dyn call`,
  [`tests/rows_scan`](./tests/rows_scan/) a minimal scan source for the builder.
- [`docs/`](./docs/) — how to use it, [`docs.dev/`](./docs.dev/) — the design agreements and the
  working rules (Japanese).

## Development

fmt, clippy and the tests are gathered in [`toolkit.nu`](./toolkit.nu). Run it before a commit, and
before calling anything done:

```nu
use toolkit.nu
toolkit check
```

It runs `fmt --check`, `clippy -D warnings` and `test`, in that order. GitHub Actions runs the same
three on every pull request.

## License

MIT ([LICENSE](./LICENSE)).

[upstream]: https://github.com/nushell/nushell/tree/main/crates/nu_plugin_polars
[seekzstdsep]: https://github.com/kazu/seekzstdsep
