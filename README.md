# nu_plugin_polars_dyn

Nushell の dataframe plugin。[nushell/nushell](https://github.com/nushell/nushell) の
`nu_plugin_polars` の fork で、Python 無しで polars をシェルから使う。本家との違い:

- コマンド接頭辞は `polars_dyn`(本家の `polars` と同時に登録できる)。
- `polars_dyn open` は scan source の registry を通す。built-in は parquet / csv / ipc / ndjson と、
  seekable zstd の `.csv.seek.zst` / `.ndjson.seek.zst`(`.jsonl.seek.zst` も同じ)で、
  読み込みオプションは `--opts` の record で渡す。
- `polars_dyn call` で polars の expression plugin(`.so`)の関数を呼ぶ。
- `polars_dyn collect --streaming` で streaming エンジンを選べる。

対応する nushell は `0.114.1`、polars は `0.55.2`。

## 構成

- `src/` — plugin 本体。`src/scan/` が registry と `polars_dyn open`、`src/call.rs` が `polars_dyn call`。
- `tests/` — `nu` を spawn する統合テスト。`tests/expr_plugin` は `polars_dyn call` 用の expression plugin。
- `dev/` — `logfmt` を compile-in した開発用 bin(publish しない、別 workspace)。
- `docs.dev/` — 設計の合意と作業規約。

## Install

```sh
cargo install nu_plugin_polars_dyn --locked
nu -c "plugin add ~/.cargo/bin/nu_plugin_polars_dyn"
```

nushell からは:

```nu
cargo install nu_plugin_polars_dyn --locked
plugin add ~/.cargo/bin/nu_plugin_polars_dyn
```

## 使い方(nushell)

```nu
plugin use polars_dyn
polars_dyn open data.csv | polars_dyn collect
polars_dyn open data.csv --opts {has_header: false} | polars_dyn collect
polars_dyn open data.parquet | polars_dyn filter ((polars_dyn col a) > 1) | polars_dyn collect --streaming
```

コマンド一覧は `help polars_dyn`、`--opts` の形は `help polars_dyn open`。

## 開発

`CARGO_TARGET_DIR` はローカルディスクに置く。ゲートは `toolkit.nu` の 1 箇所:

```nu
use toolkit.nu
toolkit check
```

`fmt --check`、`clippy -D warnings`、`test` を root と `dev/` の両方で回す。GitHub Actions は
root だけを回す(`dev/` は隣の checkout `../polars-logfmt` が要る。`docs.dev/dev_bin.md`)。
