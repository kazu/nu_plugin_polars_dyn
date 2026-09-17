# nu_plugin_polars_dyn

Nushell の dataframe plugin。[nushell/nushell](https://github.com/nushell/nushell) の
`nu_plugin_polars` の fork で、Python 無しで polars をシェルから使う。本家との違い:

- コマンド接頭辞は `polars_dyn`(本家の `polars` と同時に登録できる)。
- `polars_dyn open` は scan source の registry を通す。built-in は parquet / csv / ipc / ndjson と、
  seekable zstd の `.csv.seek.zst` / `.ndjson.seek.zst`(`.jsonl.seek.zst` も同じ)で、
  読み込みオプションは `--opts` の record で渡す。
- 自分の scan source を足したバイナリを `nu-polars-dyn-build` で作れる。
- `polars_dyn call` で polars の expression plugin(`.so`)の関数を呼ぶ。
- `polars_dyn collect --streaming` で streaming エンジンを選べる。

対応する nushell は `0.114.1`、polars は `0.55.2`。

## 構成

- `src/` — plugin 本体。`src/scan/` が registry と `polars_dyn open`、`src/call.rs` が `polars_dyn call`、
  `src/bin/nu-polars-dyn-build.rs` がカスタムバイナリのビルダー。
- `tests/` — `nu` を spawn する統合テスト。`tests/expr_plugin` は `polars_dyn call` 用の
  expression plugin、`tests/rows_scan` はカスタムバイナリ用の最小の scan source。
- `docs/` — 利用者向けの手順。
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

## 自分の scan source を足す

`polars_dyn open` が読める形式は増やせる。scan source は plugin に組み込まれるのでバイナリを
作り直すが、書くのは scan source の crate だけで、cargo project も `main.rs` も
`nu-polars-dyn-build` が生成する。

```nu
nu-polars-dyn-build my_scan_source --path my_scan_source=../my_scan_source
plugin add ./nu_plugin_polars_dyn
```

手順は [docs/custom_build.md](docs/custom_build.md)。

## 利用者が踏む制約

`AnonymousScan` で書かれた source — `.seek.zst` と、組み込んだ crate の多く — には polars の
制約がそのまま出る。parquet / csv / ipc / ndjson(plain)には当たらない。

- **`polars_dyn collect --streaming` が使えない。**polars-stream が `AnonymousScan` を
  `todo!("unimplemented: AnonymousScan")` で落とすため。plugin はこれを
  「collecting on the streaming engine: not yet implemented: unimplemented: AnonymousScan」の
  エラーにする。in-memory への fallback はしない。
- **offset 付きの `slice` が効かない。**`polars_dyn slice 1000 10` は slice が scan に降りず、
  ファイルを全部読んでから plan が 10 行取る。**検知できないので警告も出ない。**先頭から取る
  `polars_dyn slice 0 10` は効く。

`.seek.zst` にはさらに:

- **`--opts` の一部を受け付けない。**frame ごとに読むと意味が変わる 8 つ(`n_rows` /
  `skip_rows` / `skip_lines` / `skip_rows_after_header` / `row_index` / `columns` /
  `projection` / `column_names_overwrite`)はエラーになる。`polars_dyn slice` / `select` /
  `rename` が代わり。行に番号を振るものには代わりが無い。
- **schema を先頭 frame だけで決める。**後の frame にだけ現れる列は落ち、型が後で広がると
  パースに失敗する。`infer_schema_length` も先頭 frame を越えない。逃げ道は
  `--opts {schema: {fields: {n: Int64, extra: String}, metadata: null}}`。

## `.seek.zst` は何のためにあるか

追記され続けるテキストログを、ログのまま置いたまま polars で読むため。ディスクは圧縮率のぶん
減り、速度は素のファイルとおおむね同等(先頭を取るクエリだけ速い)。分析が主目的なら parquet の
方が全軸で有利。

作り方は [seekzstdsep](https://crates.io/crates/seekzstdsep) の CLI:

```nu
seekzstdsep compress events.jsonl   # → events.jsonl.seek.zst
```

対応する接尾辞は `.csv.seek.zst` / `.ndjson.seek.zst` / `.jsonl.seek.zst`。素の `.zst` は
扱わない。

## 開発

`CARGO_TARGET_DIR` はローカルディスクに置く。ゲートは `toolkit.nu` の 1 箇所:

```nu
use toolkit.nu
toolkit check
```

`fmt --check`、`clippy -D warnings`、`test` を回す。GitHub Actions も同じものを cargo 直叩きで
回す。
