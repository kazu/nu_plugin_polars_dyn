# カスタムバイナリを作る機構

自分の scan source を足した `nu_plugin_polars_dyn` を、利用者にコードを書かせずに作る。
`nu-polars-dyn-build` が cargo project を生成し、ビルドし、出来たバイナリを置く。

scan source は compile-in(理由は `agreements.md`「設計の方針」)なので、source を足すには
バイナリを作り直すしかない。作り直しに必要なのは `Cargo.toml` と 3 行の `main.rs` だけで、
利用者ごとに違うのは「どの crate を組み込むか」しかない。それを引数で受ける。

## 組み込まれる crate の規約

入口を 1 つ公開する。これだけ。

```rust
pub fn scan_sources() -> &'static [&'static dyn nu_plugin_polars::scan::ScanSource];
```

`ScanSource` の実装は `agreements.md`「`polars_dyn open` と registry」のまま。例は
`tests/rows_scan/`。

規約を満たさない crate を渡したときは、生成したコードのコンパイルエラーがそのまま出る
(`cannot find function scan_sources in crate ...`)。ビルダーは事前に検査しない — cargo と
rustc が出すものを二重に持たないため。

## コマンド

```
nu-polars-dyn-build [OPTIONS] <crate>...
```

引数は clap(derive)で解析し、説明は `--help` が出す。以下の検査は clap の外で行う。

- 位置引数は crates.io の crate 名。版は指定しない。
- `--path` / `--git` はその名前の crate をそこから取る。位置引数に無い名前、または両方に渡した
  名前はエラー。
- `--out` のデフォルトはカレントディレクトリ。既にある `nu_plugin_polars_dyn` は上書きする。
- `--debug` が無ければ release。cargo の出力はそのまま流し、失敗したらその exit code を返す。

## 生成する project

一時ディレクトリ(`$TMPDIR/nu-polars-dyn-build-<pid>`)に置く。成功したら消し、失敗したら
残してそのパスを stderr に出す。

```toml
[package]
name = "nu-polars-dyn-custom"
version = "0.0.0"
edition = "2024"
publish = false

[[bin]]
name = "nu-polars-dyn-custom"
path = "src/main.rs"

[dependencies]
nu_plugin_polars_dyn = "=<ビルダーの版>"
<利用者の crate> = "*"

[patch.crates-io]
nu_plugin_polars_dyn = { git = "...", tag = "..." }
```

```rust
fn main() {
    nu_plugin_polars::serve(&[<crate>::scan_sources(), ...]);
}
```

`serve` は env_logger の初期化、`POLARS_ALLOW_EXTENSION` の設定、built-in との連結、
`PolarsPlugin::new`、`serve_plugin` をまとめた 1 本。published の `src/main.rs` も同じものを
`&[]` で呼ぶので、生成バイナリと published バイナリの違いは引数だけになる。

### 生成 bin の名前が `nu-polars-dyn-custom` である理由

出来上がるファイルは `nu_plugin_polars_dyn` でなければならない(nushell は `nu_plugin_` で
始まる名前を plugin として扱う)が、cargo の bin 名をそれにすると、`CARGO_TARGET_DIR` を
共有したときに `<target>/<profile>/nu_plugin_polars_dyn` — この repo 自身の plugin — を
上書きする。cargo の中では別名で作り、`--out` に置くときに改名する。

### `nu_plugin_polars_dyn` を自分の出自から決める理由

ビルダーは `env!("CARGO_PKG_REPOSITORY")` と `env!("CARGO_PKG_VERSION")` を埋め込んで持つので、
利用者に聞かずに「自分と同じ版の plugin」を指せる。ビルダーとバイナリの版が食い違うと、
`serve` の署名や `ScanSource` の形が合わずコンパイルエラーになる。

**依存そのものではなく `[patch.crates-io]` に書く。**組み込む crate は自分の `Cargo.toml` で
`nu_plugin_polars_dyn = "0.1"`(crates.io)と書く。生成 project が git や path で取ると、
cargo にとっては**別 source の別 package**になり、`ScanSource` が 2 つの別の trait になって
組み合わせられない。patch にすれば、crates.io を指している依存が全部ビルダーの選んだ 1 つに
解決される。未 publish でも効くことは実測で確かめた(`tests/custom_build.rs` の
`a_crate_taking_the_plugin_from_crates_io_builds`)。

**この形は、その版の tag が push されていて初めて動く。**release ごとに、`Cargo.toml` の
`version` を上げた commit に `<version>` の tag(`v` 無し)を打つこと。tag の版と `version` が
食い違うと、ビルダーは `version` の方の tag を指す(`0.1.1` の tag は `version = "0.1.0"` の
commit に打たれていて、その形になっている)。

環境変数 `NU_POLARS_DYN_SOURCE` にディレクトリを渡すと、そこへの path 依存に差し替わる。
tag がまだ無い版に対して使うための逃げ道で、`tests/custom_build.rs` はこれで手元の checkout に
対してビルドしている。

`--path` の値も `NU_POLARS_DYN_SOURCE` も、生成 project に書く前に絶対パスにする。cargo は
`path` 依存をそれを書いた manifest 基準で解決するので、利用者のカレントディレクトリ基準の
相対パスをそのまま書くと一時ディレクトリ基準になって外れる。

### `Cargo.lock` を埋め込む理由

生成 project には、ビルダーがビルドされたときの `Cargo.lock` を置く。cargo は既にある版を
優先するので、カスタムバイナリは published バイナリと**同じ依存の版**で組まれる。

副作用として、その版のビルド成果物が `CARGO_TARGET_DIR` に既にあれば再利用される。lock 無しで
解決させると手元の実測で 1 回目が 1m51s、lock 込みだと 15s だった。

## テスト

`tests/common/mod.rs` がバイナリを 1 本作るところを持ち、2 つのテストが使う。

- `tests/custom_build.rs` — `tests/rows_scan`(`.rows` を 1 列の `line` に読む最小の source)を
  組み込み、built-in が残っていること、`--opts` が crate まで届くこと、規約を満たさない crate が
  コンパイルエラーで落ちることを見る。
- `tests/open_seek_zst.rs` — `seekzstdsep-scan` を組み込み、`.seek.zst` が plain と同じ答えを
  返すことを 17 のクエリで見る。バイナリはファイルにつき 1 本で、`OnceLock` で共有する。

**1 つのテストファイルが呼ぶビルドは 1 本にまとめてある。**生成 project のビルドは cargo の
入れ子呼び出しなので、同じファイルのテストが並行すると cargo が 2 つ走る。まとめれば直列になり、
2 回目のビルドが 1 回目の成果物を使える。

バイナリの置き場は `<target>/custom-plugin-tests/<名前>/` で、一時ディレクトリではない。
`OnceLock` に入れた `TempDir` は drop されないので、一時ディレクトリだと `cargo test` のたびに
`$TMPDIR` に 1 本ずつ残る。決まった場所に置けば上書きされる。
