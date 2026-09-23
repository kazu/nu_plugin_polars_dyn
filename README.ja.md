# nu_plugin_polars_dyn

[![CI](https://img.shields.io/github/actions/workflow/status/kazu/nu_plugin_polars_dyn/ci.yml?branch=main)](https://github.com/kazu/nu_plugin_polars_dyn/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)

**読み込めるフォーマットを自分で追加できる、nushell の dataframe plugin です。**

nushell の [`nu_plugin_polars`][upstream] の fork です。パイプを流れるのは遅延したプランで、
`polars_dyn open` は `LazyFrame` を返し、後ろのコマンドはプランを足すだけ、`polars_dyn collect`
まで何も読みません。filter や列の選択は読み込みの段階で効きます。Python はどこにも要りません。

| nushell | polars |
| --- | --- |
| 0.114.1 | 0.55.2 |

## 目次

- [本家との違い](#本家との違い)
- [インストール](#インストール)
- [利用方法](#利用方法)
- [カスタムスキャナの追加方法](#カスタムスキャナの追加方法)
  - [同梱の `.seek.zst` と `ssh://`](#同梱の-seekzst-と-ssh)
- [ディレクトリ構成](#ディレクトリ構成)
- [開発](#開発)
- [ライセンス](#ライセンス)

## 本家との違い

- **[`polars_dyn open`](docs/cli.md#polars_dyn-open)** — フォーマットごとの分岐を scan source の
  **chain** に置き換えました。`ssh://host/log/events.jsonl.seek.zst` は `ssh` → `seek-zst` →
  `ndjson` の 3 段で、scheme が開き、接尾辞が順に展開と読み込みを担います。どの段も前後を
  知らないので、開き方・圧縮・フォーマットは何とでも組み合わさります。chain は `--format`、
  各段のオプションは `--opts` の record で渡します。
- **カスタムスキャナ** — scheme(`ssh://`)、圧縮(`.seek.zst`)、フォーマット(`.logfmt`)の
  どれも、自分で書いた Rust の crate で追加できます。[`nu-polars-dyn-build`](docs/cli.md#nu-polars-dyn-build)
  がそれを組み込んだ**自分用の plugin バイナリ**を作ります。何本でも組み込めます。
- **[`polars_dyn call`](docs/cli.md#polars_dyn-call)** — polars の expression plugin(`.so`)が
  公開する関数を式として呼べます。
- **[`polars_dyn collect --streaming`](docs/cli.md#polars_dyn-collect---streaming)** — polars の
  streaming エンジンで collect できます(parquet / ipc)。
- **接頭辞 `polars_dyn`** — 本家の `polars` plugin と同じ nushell に同時に登録できます。

引き継いだコマンド(`filter` / `select` / `group-by` / `sort-by` など)は本家のままです。
考え方は nushell の本の [Dataframes](https://www.nushell.sh/book/dataframes.html)、一覧は
[dataframe コマンド](https://www.nushell.sh/commands/categories/dataframe.html)(向こうの
接頭辞は `polars`)。手元では `help polars_dyn` で引けます。

## インストール

crates.io にはまだ無いので、リポジトリからビルドします:

```sh
git clone https://github.com/kazu/nu_plugin_polars_dyn
cd nu_plugin_polars_dyn
cargo install --path . --locked
```

plugin と、ビルダーの `nu-polars-dyn-build` が入ります。nushell から登録します:

```nu
plugin add ~/.cargo/bin/nu_plugin_polars_dyn
plugin use polars_dyn
```

## 利用方法

```nu
polars_dyn open data.csv | polars_dyn collect
polars_dyn open data.csv --opts {csv: {has_header: false}} | polars_dyn collect
polars_dyn open data.txt --format csv | polars_dyn collect
polars_dyn open data.parquet | polars_dyn filter ((polars_dyn col a) > 1) | polars_dyn collect --streaming
polars_dyn open events.jsonl.seek.zst | polars_dyn collect                              # seekzstdsep_scan を組み込んだバイナリ
polars_dyn open ssh://host/var/log/events.jsonl.seek.zst | polars_dyn first 10 | polars_dyn collect   # ssh_scan も
[[a]; [1] [2]] | polars_dyn into-df | polars_dyn select (polars_dyn call ./libmy_plugin.so add a --kwargs {n: 10} --elementwise) | polars_dyn collect
```

それぞれのコマンドで使えるオプションは [`polars_dyn open`](docs/cli.md#polars_dyn-open)、
[`polars_dyn collect --streaming`](docs/cli.md#polars_dyn-collect---streaming)、
[`polars_dyn call`](docs/cli.md#polars_dyn-call)、
[`polars_dyn store-ls` / `store-rm`](docs/cli.md#polars_dyn-store-ls--store-rm) にあります。

## カスタムスキャナの追加方法

`polars_dyn open` の chain の段 — 開く(scheme)、包む(圧縮など)、読む(フォーマット)— は
自分で追加できます。scan source は関数を 1 つ公開する lib crate で、cargo project も `main.rs` も
`nu-polars-dyn-build` が書きます。

```rust
pub fn scan_sources() -> &'static [&'static dyn nu_plugin_polars::scan::ScanSource];
```

```nu
nu-polars-dyn-build my_scan_source --path my_scan_source=../my_scan_source
plugin add ./nu_plugin_polars_dyn
```

crate は何本でも並べられるので、複数のカスタムスキャナを 1 つのバイナリに組み込めます:

```nu
nu-polars-dyn-build seekzstdsep_scan ssh_scan my_scan_source --path seekzstdsep_scan=./seekzstdsep-scan --path ssh_scan=./ssh-scan --path my_scan_source=../my_scan_source
```

出来たバイナリは標準のものを置き換え、built-in のフォーマットもそのまま使えます。crate の
書き方、ビルダーのオプション、`AnonymousScan` で踏む制約(`--streaming` で collect できない、
offset 付きの `slice` が読み込みに効かない)は [docs/custom_build.md](docs/custom_build.md)。

### 同梱の `.seek.zst` と `ssh://`

[`seekzstdsep-scan/`](./seekzstdsep-scan/) と [`ssh-scan/`](./ssh-scan/) はそのまま組み込める
scan source です。前者は [seekzstdsep][] が書く seekable zstd のファイル(`.seek.zst`)を展開後の
バイト列として次の段に渡し、後者は `ssh://[user@]host/path` を sftp で開きます。どちらも
built-in の csv / ndjson / parquet / ipc と、他の crate のフォーマットの前に付きます。

```nu
seekzstdsep compress events.jsonl   # -> events.jsonl.seek.zst
nu-polars-dyn-build seekzstdsep_scan ssh_scan --path seekzstdsep_scan=./seekzstdsep-scan --path ssh_scan=./ssh-scan
polars_dyn open events.jsonl.seek.zst | polars_dyn collect
polars_dyn open ssh://host/var/log/events.jsonl.seek.zst --opts {ssh: {port: 2222}} | polars_dyn collect
```

`.seek.zst` は、追記され続けるテキストログをログのまま置いたまま polars で読むためにあります。
ディスクは圧縮率のぶん減り、読む速度は素のファイルとおおむね同等です。分析だけが目的の
ファイルなら parquet の方が全軸で有利です。`ssh` の `--opts` は `{port, identity}` で、
`identity` が無ければ ssh-agent で認証します。

logfmt のログは [polars-logfmt][] の `logfmt-scan` が `.logfmt` の段になります(別 repo。
`--path logfmt_scan=<polars-logfmt の checkout>/logfmt-scan` で組み込む)。

## ディレクトリ構成

- [`src/`](./src/) — plugin 本体。[`src/scan/`](./src/scan/) が registry と `polars_dyn open`、
  [`src/call.rs`](./src/call.rs) が `polars_dyn call`、
  [`src/bin/nu-polars-dyn-build.rs`](./src/bin/nu-polars-dyn-build.rs) がビルダー。
- [`seekzstdsep-scan/`](./seekzstdsep-scan/) — `.seek.zst` の scan source、
  [`ssh-scan/`](./ssh-scan/) — `ssh://` の scan source。plugin 本体には入りません。
- [`tests/`](./tests/) — `nu` を spawn する統合テスト。
  [`tests/expr_plugin`](./tests/expr_plugin/) は `polars_dyn call` 用の expression plugin、
  [`tests/rows_scan`](./tests/rows_scan/) はビルダー用の最小の scan source。
- [`docs/`](./docs/) — 利用者向けの手順(英語)、[`docs.dev/`](./docs.dev/) — 設計の合意と作業の
  規約。

## 開発

fmt / clippy / test は [`toolkit.nu`](./toolkit.nu) にまとめてあります。commit の前と、終わったと
言う前にこれを回します。

```nu
use toolkit.nu
toolkit check
```

`fmt --check`、`clippy -D warnings`、`test` の順です。GitHub Actions が pull request ごとに
回すのも同じ 3 つです。

## ライセンス

MIT ([LICENSE](./LICENSE))。

[upstream]: https://github.com/nushell/nushell/tree/main/crates/nu_plugin_polars
[seekzstdsep]: https://github.com/kazu/seekzstdsep
[polars-logfmt]: https://github.com/kazu/polars-logfmt
