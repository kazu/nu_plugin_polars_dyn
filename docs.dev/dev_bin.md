# `dev/` bin: logfmt を compile-in した plugin

`polars_dyn open x.logfmt` を動かすための、publish しない bin。published crate(root)は
`polars_logfmt` に依存せず built-in の parquet / csv / ipc / ndjson だけを登録する。
`dev/` は同じ lib に `polars_logfmt` を path 依存で足し、`logfmt` を scan registry に登録して
`serve_plugin` する。設計は [agreements.md](./agreements.md)「`Cargo.toml` の独立と polars 版」。
logfmt が `.so` 経由で呼べるようになったら `dev/` は消す。

## 構成

- `dev/Cargo.toml` — workspace member、`publish = false`、
  `polars_logfmt = { path = "../../polars-logfmt/polars-logfmt" }`(同じ親ディレクトリに
  `polars-logfmt` の checkout があること)。
- `dev/src/main.rs` — `BUILTIN` に `Logfmt` を足した slice を `PolarsPlugin::new` に渡すだけ。
- `dev/src/logfmt.rs` — `ScanSource` の impl。`name = "logfmt"`、接尾辞 `.logfmt` / `.logfmt.zst`。
  `--opts` は `polars_logfmt::LogfmtScanOpts` の JSON(`line_filter` は部分文字列、`schema`、
  `batch_size`、`n_threads`、`aligned_cols_cnt`、ssh の `cmd`)。
- `dev/tests/open_logfmt.rs` — `.logfmt` / `.logfmt.zst` の統合テスト。`cargo test --workspace`
  で走る。

## ビルドと登録

bash / zsh:

```sh
cargo build -p nu_plugin_polars_dyn_dev
nu -c "plugin add $CARGO_TARGET_DIR/debug/nu_plugin_polars_dyn_dev"
```

nushell:

```nu
cargo build -p nu_plugin_polars_dyn_dev
plugin add $"($env.CARGO_TARGET_DIR)/debug/nu_plugin_polars_dyn_dev"
```

登録せずに 1 回だけ使うなら `nu --plugins [<path>]`。published bin と同時に登録すると
コマンド名(`polars_dyn ...`)が衝突するので、どちらか一方だけ登録する。

## 使い方(nushell)

```nu
polars_dyn open app.logfmt | polars_dyn collect
polars_dyn open app.logfmt --opts {line_filter: "level=error"} | polars_dyn collect
polars_dyn open app.logfmt.zst | polars_dyn collect
polars_dyn open ssh://user@host/var/log/app.logfmt --opts {cmd: "cat /var/log/app.logfmt"} | polars_dyn collect
```

`ssh://` は `cmd` 無しなら SFTP(ssh-agent 認証)、`cmd` 有りならその command の stdout を読む。
`.logfmt.zst` は seekzstdsep 形式の seekable zstd。`polars_dyn collect --streaming` は
`AnonymousScan` を polars-stream が扱えないため logfmt では使えない(agreements.md
「`CustomValue` と cache」)。
