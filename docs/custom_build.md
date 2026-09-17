# 自分の scan source を足したバイナリを作る

`polars_dyn open` が読める形式を増やす手順。scan source は plugin に組み込まれるのでバイナリを
作り直すが、書くコードは scan source の crate だけで、cargo project も `main.rs` も
`nu-polars-dyn-build` が生成する。

この文書はコマンドの並び。なぜこの形かは `docs.dev/custom_build.md`。

## 1. ビルダーを用意する

`nu-polars-dyn-build` は plugin と同じ crate に入っている。

```nu
cargo install nu_plugin_polars_dyn --locked
```

repo から入れるなら:

```nu
cd <この repo>
$env.CARGO_TARGET_DIR = ($env.HOME | path join ".cargo" "target")
cargo build --release --bin nu-polars-dyn-build
```

`~/.cargo/target/release/nu-polars-dyn-build` が出来る。

## 2. scan source の crate を書く

crate が公開する入口は 1 つだけ。

```rust
pub fn scan_sources() -> &'static [&'static dyn nu_plugin_polars::scan::ScanSource];
```

`ScanSource` は形式 1 つを `LazyFrame` にする trait で、実装するのは 3 つのメソッド。

```rust
use nu_plugin_polars::scan::{ScanSource, parse_opts};
use polars::prelude::{LazyFrame, PolarsResult};

pub fn scan_sources() -> &'static [&'static dyn ScanSource] {
    &[&MyFormat]
}

struct MyFormat;

impl ScanSource for MyFormat {
    /// `--format` に渡せる名前。
    fn name(&self) -> &'static str {
        "myformat"
    }

    /// `--format` が無いとき、source の末尾と最長一致で選ばれる。`.` で始める。
    fn suffixes(&self) -> &'static [&'static str] {
        &[".myfmt"]
    }

    /// `source` は絶対パスか、scheme 付きならその URL のまま。`opts` は `--opts` の record を
    /// JSON にした bytes(省略時は空)。collect はしない。
    fn scan(&self, source: &str, opts: &[u8]) -> PolarsResult<LazyFrame> {
        let opts = parse_opts(opts)?; // serde_json::Map
        todo!()
    }
}
```

`Cargo.toml` はふつうの lib crate でよく、`nu_plugin_polars_dyn` を依存に入れる
(lib 名は `nu_plugin_polars`)。

```toml
[dependencies]
nu_plugin_polars_dyn = "0.1"
polars = "=0.55.2"
```

動く最小の実装は repo の `tests/rows_scan/src/lib.rs`、実際の形式を読むものは
`polars-logfmt` の `logfmt-scan/src/lib.rs`。

規約を満たさない crate を渡すと、生成したコードのコンパイルエラーがそのまま出る
(`cannot find function scan_sources in crate ...`)。

## 3. ビルドする

```nu
nu-polars-dyn-build my_scan_source
```

カレントディレクトリに `nu_plugin_polars_dyn` が出来る。位置引数は crates.io の crate 名で、
版は指定しない。まだ tag の無い版に対して作るときは下の節も読むこと。publish していない
crate は取り方を渡す:

```nu
nu-polars-dyn-build my_scan_source --path my_scan_source=../my_scan_source
nu-polars-dyn-build my_scan_source --git my_scan_source=https://example.com/my_scan_source
```

crate は何本でも並べられる:

```nu
nu-polars-dyn-build a b c --path a=../a --git b=https://example.com/b
```

| flag | 意味 |
|---|---|
| `--path <name>=<dir>` | その crate をディレクトリから取る |
| `--git <name>=<url>` | その crate を git から取る |
| `--out <dir>` | 置き場所。既定はカレントディレクトリ |
| `--debug` | debug ビルド。既定は release |

cargo の出力はそのまま流れ、失敗したらその exit code が返り、生成した project は消さずに
パスを stderr に出す。`CARGO_TARGET_DIR` を設定しておくと 2 回目以降のビルドが短くなる。

### まだ tag の無い版に対してビルドする

生成 project は `nu_plugin_polars_dyn` を、ビルダー自身がビルドされた repo の、自分と同じ版の
tag から取る。**その tag が push されていない版では解決できない。**手元の checkout を渡す:

```nu
$env.NU_POLARS_DYN_SOURCE = "/path/to/nu_plugin_polars_dyn"
nu-polars-dyn-build my_scan_source --path my_scan_source=../my_scan_source
```

`0.1.0` はまだ tag が無いので、いまはこの形で使う。

## 4. 登録して使う

```nu
plugin add ./nu_plugin_polars_dyn
plugin use polars_dyn
polars_dyn open data.myfmt | polars_dyn collect
polars_dyn open data.myfmt --opts {my_option: 10} | polars_dyn collect
polars_dyn open data.csv | polars_dyn collect      # built-in もそのまま
```

出来たバイナリは標準の `nu_plugin_polars_dyn` を**置き換える**もので、built-in の
parquet / csv / ipc / ndjson / `.seek.zst` はそのまま使える。

`plugin add` は登録した時点の署名を保存するので、バイナリを入れ替えたら `plugin add` を
やり直す。試すだけなら registry を使わずに叩ける:

```nu
nu --plugins [./nu_plugin_polars_dyn] -c "polars_dyn open data.myfmt | polars_dyn collect"
```

## 踏む制約

自前の scan source は多くの場合 polars の `AnonymousScan` になる。そのとき:

- **`polars_dyn collect --streaming` が使えない。**polars-stream が `AnonymousScan` を
  `todo!` で落とすため、エラーになる(in-memory への fallback はしない)。
- **offset 付きの `slice` が `scan` に降りない。**`polars_dyn slice 1000 10` は全部読んでから
  plan が 10 行取る。**検知できないので警告も出ない。**`polars_dyn slice 0 10` は降りる。
- **`scan` が受け取る `n_rows` は predicate より先に当てる。**ファイルの行に対する指定なので、
  逆にすると黙って誤答する。
- **評価できない predicate が来ることがある。**`sort` + `slice` の動的な上限・下限は
  optimizer の内部表現で届き、`LazyFrame::filter` に渡すとプロセスごと落ちる。`and` の項ごとに
  分けて、その項だけ外す(外して正しい。`sort` 側が自分の slice を持っている)。
