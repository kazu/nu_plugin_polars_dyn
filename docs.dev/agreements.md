# 合意事項

設計の議論で決まったことだけを書く。未決の論点は末尾に分けて置く。

## 目的

nushell から、Rust 製の polars 拡張を自由に使えるようにする。expression の拡張は polars 公式の
expression plugin(動的ライブラリ `.so` / `.dll`)を FFI 経由でそのまま、scan の拡張は
利用者の crate を組み込んだバイナリを作る形で。polars のオブジェクトは遅延評価(LazyFrame)の
ままパイプラインに流す。基本目標は **Python 無しで polars をシェルとして使える**ことで、
Python 向けに作られた拡張の互換は副次的。

## 名前

- nu 側のコマンド接頭辞は `polars_dyn`(`polars_dyn open` など)。crate 名とバイナリ名は
  nu plugin の慣習どおり `nu_plugin_polars_dyn`。
- 本家 `nu_plugin_polars` とは**完全に別の plugin**として同居できるようにする。
  コマンド接頭辞 `polars` は使わない。短く打ちたい場合は nu 側の alias で解く。

## 出自

- nushell tag `0.114.1` の `crates/nu_plugin_polars` を `git subtree split` で
  切り出した履歴(297 commit)を `main` の起点にする。切り出し元の blame が追える。
- 本家(nushell / nu-polars)への働きかけはしない。解決はこの repo の中で完結させる。

## 設計の方針

- 本家 `polars open` にあるファイル形式ごとの分岐と個別パースは使わない。
  パスをそのまま polars 本体(または registry の scan 実装)に渡して LazyFrame を
  返す抽象化コマンドで置き換える。名前は nushell の `open` に合わせて
  `polars_dyn open`(polars の API 名 `scan` は使わない)。
- polars のオブジェクトは nushell の `CustomValue` で不透明な型として流す。
  LazyFrame / Expr / DataFrame のどれでも内包できる enum のコンテナは 1 つ(本家の
  `PolarsPluginObject`)。ポインタの生操作はしない。
- 拡張のロードは nushell の別 plugin ではなく **この plugin プロセスの中**で行う。
  nushell のパーサやコマンド登録の制約を回避し、別 plugin 間で `CustomValue` を
  渡せない問題を根本から避ける。
- expression の拡張は polars 公式の expression plugin 機構をそのまま使う
  (`polars-plan` の feature `ffi_plugin`、`FunctionExpr::FfiPlugin`)。`.so` は
  polars の loader に開かせ、自前の FFI は書かない。
- scan の拡張は **compile-in** にする。fork 側に「名前 → scan 実装」の registry を 1 つ置き、
  `polars_logfmt` 等は bin が cargo 依存として registry に登録する。利用者が自分の scan
  source を足したバイナリを作れるように、その cargo project を生成してビルドするコマンドを
  fork が持つ(「カスタムバイナリを作る」の節)。
- scan の `.so` ロードはしない。polars に scan 用の `.so` 機構は無く(`AnonymousScan` は
  同一バイナリ内の trait object)、自前で作ると Rust の型を別々のビルド成果物のあいだで
  渡すことになる。`repr(Rust)` のレイアウトはコンパイルごとに変わりうるので言語として
  保証が無く、実測でも polars の global がバイナリごとに二重になり、片方が作った `Schema` を
  もう片方が引けずクエリが黙って誤答した(task 010 の記録)。採らなかった案は
  自前 C ABI + Arrow C Data Interface(projection と predicate が `.so` に降りない)、
  別プロセス + Arrow IPC stream(同じく降りず、コピーとプロセス起動が乗る)。
- registry の境界の入力は source の文字列とオプションの bytes に保つ。`--opts` の中身を
  nu 側で覗かない、という一点だけが理由で、FFI を越えるためではない。

## `polars_dyn open` と registry

```
polars_dyn open <source: string> [--format (-f) <name>] [--opts (-o) <record>]  → LazyFrame
```

- `source` は文字列をそのまま実装に渡す(ローカルパス、URL、logfmt の ssh 指定など)。
  nu 側では解釈しない。
- `--format` が無ければ、各実装が宣言する接尾辞の最長一致で決める
  (`app.logfmt.seek.zst` → `logfmt`、`x.parquet` → `parquet`)。決まらなければ登録名を
  列挙してエラー。
- `--opts` は record を JSON bytes にして実装へ渡す。中身は実装ごとの契約で、nu 側は
  覗かない。省略時は空。形式固有の flag(`--delimiter` 等)は付けない。`--eager` も
  付けない(`collect` で足りる)。
- registry の境界は trait 1 つ。返り値は LazyFrame で、collect はしない。

  ```rust
  pub trait ScanSource: Send + Sync {
      fn name(&self) -> &'static str;                 // "logfmt"
      fn suffixes(&self) -> &'static [&'static str];  // [".logfmt", ".logfmt.seek.zst"]
      fn scan(&self, source: &str, opts: &[u8]) -> PolarsResult<LazyFrame>;
  }
  ```

- registry は `PolarsPlugin` 構築時に bin から渡す `&[&dyn ScanSource]`。built-in は
  **polars 自身が読む parquet / csv / ipc / ndjson の 4 つだけ**で、`--opts` の JSON を
  `polars-io` のオプション struct(`CsvReadOptions` 等。`serde` feature で `Deserialize` を
  derive している)に直接食わせる。形式ごとの flag 解析を fork には持たない。
  `.csv.seek.zst` / `.ndjson.seek.zst` / `.jsonl.seek.zst` は `seekzstdsep-scan`、`logfmt` は
  `logfmt-scan`(別 repo)の crate で、どちらも `nu-polars-dyn-build` が組み込む。
  接尾辞の衝突は構築時に 1 回検査する。
- logfmt の `opts` は serde struct(`line_filter` は closure を渡せないので部分文字列)。
- 採らなかった案: source ごとの named flag(`open` の分岐を作り直すことになる)、
  `inventory` crate による自動収集(構築時に渡す配列で足りる)、一覧コマンド
  (エラーメッセージの列挙で足りる)。
- 本家の `open.rs`(形式ごとの分岐と flag 群)は**皮も残さず消す**。新 `open` は
  ブランチで書き、`--opts` で built-in 形式が賄えると確認できた時点で旧 `open.rs` と
  差し替える。それまで main では旧 `open` が動いている。

## カスタムバイナリを作る

registry に渡す配列は bin が組む。published バイナリは built-in だけを渡すので、自分の scan
source を足したい利用者は**自分のバイナリを作る**。そのための cargo project を利用者に書かせず、
fork が生成してビルドする。

```
nu-polars-dyn-build <crate>... [--path <name>=<dir>]... [--git <name>=<url>]...
                    [--out <dir>] [--debug]
```

- **利用者が書くコードはゼロ。**`Cargo.toml` も `main.rs` もビルダーが生成する。出来るバイナリの
  名前は `nu_plugin_polars_dyn` のまま(nushell は `nu_plugin_` で始まる名前を plugin として
  扱う)。ビルダーは plugin ではないので `nu-polars-dyn-build`。published バイナリを置き換える。
- 組み込まれる crate は入口を 1 つ公開する。これだけが規約で、`ScanSource` の実装は上の節のまま。

  ```rust
  pub fn scan_sources() -> &'static [&'static dyn nu_plugin_polars::scan::ScanSource];
  ```

  規約を満たさない crate を渡した場合は、生成したコードのコンパイルエラーがそのまま利用者に出る。
  ビルダーは事前に検査しない(cargo と rustc が出すものを二重に持たない)。
- 位置引数は crates.io の crate 名で、版は指定しない。`--path` / `--git` はその名前の crate を
  そこから取る逃げ道で、publish していない crate のために要る。
- 生成 project が依存する `nu_plugin_polars_dyn` は、**ビルダーが自分の出自を埋め込んで**
  決める。`env!("CARGO_PKG_REPOSITORY")` を git URL に、`env!("CARGO_PKG_VERSION")` を tag に
  した git 依存で、利用者は何も渡さない。ビルダーとバイナリの版が食い違わないため。
  環境変数 `NU_POLARS_DYN_SOURCE` にディレクトリを渡すとそこへの path 依存に差し替わる —
  publish 前と、この repo 自身の統合テストのための逃げ道。採らなかった案は利用者に path を
  渡させること(ビルダーが自分の出自を知っているのに聞く理由が無い)。
- `bin` は `nu_plugin_polars::serve(extra)` を呼ぶだけ。`serve` は env_logger の初期化、
  `POLARS_ALLOW_EXTENSION` の設定、`BUILTIN` と `extra` の連結、`PolarsPlugin::new` と
  `serve_plugin` をまとめた 1 本で、published の `src/main.rs` も同じものを呼ぶ。

## `polars_dyn call`(expression plugin)

```
polars_dyn call <lib: path> <symbol: string> ...<args: expr>
    [--kwargs (-k) <record>] [--elementwise] [--returns-scalar] [--changes-length]
    [--cast-to-supertype] [--pass-name]  → Expr
```

- `.so` はパスをコマンドに直接渡す。ディレクトリ走査や名前登録はしない(Python の
  `register_plugin_function(plugin_path=...)` と同じ)。短く打ちたければ nu の `const` や
  alias で解く。採らなかった案: 環境変数のディレクトリ + 名前解決(registry が増え、
  どのファイルが呼ばれたか隠れる)。
- 相対パスは `engine.get_current_dir()` で plan に入れる前に絶対化する。dlopen は
  collect 時に polars の loader が行うので、cwd が変わっても同じファイルを指すように。
- `symbol` は `.so` が export する `_polars_plugin_<symbol>` の `<symbol>` 部分。出力 dtype
  は polars が `_polars_plugin_field_<symbol>` で決めるので、コマンド側で指定しない。
- `--kwargs` は record を pickle bytes にして渡す(`serde-pickle` crate)。`pyo3-polars` の
  `#[polars_expr]` が kwargs を pickle で復号するので、既存の expression plugin をそのまま
  呼ぶにはこれしかない。自作の `.so` も同じ形にして 1 種類で済ませる。
- switch 群は Python の `register_plugin_function` の引数と 1 対 1。
  `input_wildcard_expansion` は付けない(要ったら足す)。
- 実装は `Expr::Function { input, function: FunctionExpr::FfiPlugin { flags, lib, symbol,
  kwargs } }` を組むだけ(`polars-python` の `register_plugin_function` の写し)。自前の
  FFI は無し。
- 未検証: `pyo3-polars` で作った `.so` は `pyo3` を必ず link するので、Python の無い
  plugin プロセスで dlopen できるかは不明。Python 向け拡張の互換は副次目標なので後で
  考える。自作の `.so` は `polars-ffi` + `serde-pickle` だけで書けるので影響しない。

## `CustomValue` と cache

- 「UUID → 本体」の表は残す。`AnonymousScan` を含む LazyFrame は trait object を抱えて
  直列化できない(上流 issue #20602)ので、plugin プロセスの外へ出せるのは handle だけ。
- 消すもの: 参照カウント、`custom_value_dropped` の drop 通知、insert / remove ごとの
  plugin GC の on / off(`set_gc_disabled` の往復)、`store-get`。plugin GC は起動時に
  1 回 off にする。値は `store-rm` で明示的に消すか plugin 終了まで生きる。
- 残すもの: `NuDataFrame` / `NuLazyFrame` / `NuExpression` 等の値型、nu の table と
  DataFrame の相互変換、`store-ls` / `store-rm`(参照カウントの代わりの手動の掃き出し口)。
- eager / lazy の制御は現状維持。明示の切り替え(`collect` / `into-lazy` / `into-df`)と、
  入力の種類が出力に引き継がれる規則(`from_eager` / `from_lazy` の flag と
  `cache_and_to_value` の判定)はそのまま残す。
- 例外は 1 つ: `polars_dyn collect --streaming` で polars の streaming エンジン
  (`Engine::Streaming`、`polars-stream`)を明示的に選べる。デフォルトは in-memory のままで、
  暗黙の collect(`into-nu` に lazy を渡す、`shape`、`summary` 等)は触らない。
  `Cargo.toml` の polars に feature `streaming` を足すので、環境変数 `POLARS_FORCE_STREAMING=1`
  も併存して効く。採らなかった案: デフォルトを streaming にする(未対応ノードがエラーになる)、
  全コマンドに flag を撒く(先に `collect --streaming` を挟めば足りる)、環境変数だけに頼る
  (プロセス全体に効いて暗黙の collect も巻き込む)。
  `AnonymousScan` で書かれた source(logfmt、seek.zst の frame 層)は streaming で collect
  **できない**。polars-stream 0.55.2 は `FileScanIR::Anonymous` を `todo!("unimplemented:
  AnonymousScan")` で落とす(0.52 から変わっていない)。plugin はその panic を捕まえて
  「`collecting on the streaming engine: not yet implemented: unimplemented: AnonymousScan`」の
  エラーとして返し、in-memory へ黙って fallback しない(polars 自身は `POLARS_AUTO_STREAMING=1`
  のときだけ fallback する)。文言は `tests/collect_streaming.rs` で固定。
  `streaming` feature を入れると in-memory エンジンも file scan だけは polars-stream に委ねるので、
  `POLARS_VERBOSE=1` の `polars-stream:` 行は両エンジンで出る。エンジンの区別は in-memory だけの
  `run sink_mem` と streaming だけの `running streaming-slice in subgraph` で見る。

## `Cargo.toml` の独立と polars 版

- **crates.io に publish する**(他の人が `cargo install` で入れられるように)。crate 名は
  nu plugin の慣習どおりバイナリ名と同じ `nu_plugin_polars_dyn`(crates.io で空き確認済)。
  `version = "0.1.0"`、`edition = "2024"`、`rust-version = "1.95.0"`、`license = "MIT"`
  (本家の LICENSE は残す)、`authors` と `repository` は kazu のもの。
- publish する manifest には path / git 依存を置けない(cargo が版の無い依存を拒む)ので、
  logfmt のような非公開の scan source は published crate に入れない。registry は静的配列では
  なく **`PolarsPlugin` 構築時に bin が渡す**形にして、published バイナリは built-in だけを、
  `nu-polars-dyn-build` が生成するバイナリは built-in + 利用者の crate を渡す。
  生成 project は published manifest ではないので path / git 依存を置ける。
- `workspace = true` は nushell 0.114.1 の root の実値で置き換える。nu 系は crates.io の
  `=0.114.1`(`nu-protocol` / `nu-plugin` / `nu-path` / `nu-utils`、dev の `nu-cmd-lang` /
  `nu-engine` / `nu-parser` / `nu-command` / `nu-plugin-test-support`)。feature は本家の
  指定どおり(`os`、dev 側は `plugin`)。その他は `chrono 0.4.42`(default-features 無し +
  `unstable-locales`)、`fancy-regex 0.18`、`log 0.4`、`url 2.5`、`tempfile 3.27`。
  `tokio` の直接依存は cloud 認証層と一緒に消える(polars が内部で持つ分は残る)。
- `Cargo.lock` を commit する。`CARGO_TARGET_DIR` は repo に書かずセッションの env で渡す。
- `polars-lazy` に feature `ffi_plugin` を付ける(facade の `polars` にはこの feature が無い)。
- polars 版の順序: (1) `=0.54.4` のまま単体ビルドを緑にする(独立化と版上げを混ぜない)
  → (2) `=0.55.2` へ上げる(nushell main が既に上げているので、その差分を取り込む)
  → (3) `polars_logfmt` を 0.52 から 0.55.2 へ一度で上げる。logfmt 側 repo で、
  workspace の member から外れている問題と `bigidx` 等の feature 統合の影響もこの段で
  直す → (4) `nu-polars-dyn-build` で結線。(1)(2) は logfmt に依存しない。
- 以後 fork の polars 版は nu のリリースではなく、logfmt と揃うことを基準に動かす。

## `CustomValue` コンテナ enum

- 本家の `PolarsPluginObject` をそのまま使う。変種は `NuDataFrame` / `NuLazyFrame` /
  `NuExpression` / `NuLazyGroupBy` / `NuWhen` / `NuDataType` / `NuSchema` / `NuSelector`。
  Series(eager)は本家どおり 1 列の `NuDataFrame` で表し、変種を足さない。
- `NuPolarsTestData` は消す(cache の参照カウントのテスト専用)。
- 名前も変えない。**消さないコードは触らない**。残すコードは本家との差分を最小に保ち、
  上流の修正(polars 版の追従など)を cherry-pick できる状態にする。これは fork 全体の
  方針。

## 消すもの・残すもの

消す:
- `open.rs`、cache の参照カウント・GC 往復・`store-get`、`NuPolarsTestData`(上の各節)。
- plugin 側の cloud 認証層: `src/cloud/`(aws / azure / gcp)、`resource.rs` が scheme ごとに
  認証を組む処理(scheme 付きの path には `CloudOptions::default()` を渡し、認証は polars が
  環境変数から拾う。`save` の cloud sink の経路はそのまま)、
  `aws-config` / `aws-credential-types`、`PolarsPlugin` が抱える tokio の `Runtime`。
  この層の価値は「AWS profile / SSO でも読める」の 1 点だけで、そのために AWS SDK と
  tokio を背負っている。

残す(触らない):
- polars の feature `cloud` / `aws` / `azure` / `gcp`。S3 等の読み込みは polars 自身の機能で、
  認証情報は `object_store` が環境変数から拾う。profile / SSO が要る人は環境変数へ export
  すればよい(`aws configure export-credentials --format env`)。`--opts` に
  `cloud_options` を書けば `CloudOptions` の serde で通る(実装時に確認)。
  採らなかった案は `cloud` feature も落とすことで、依存木は軽くなるが publish 後に S3 を
  読みたい利用者を切る。
- コマンド群すべて(aggregation / boolean / computation / data / datetime / index /
  integer / list / selector / string)。これが fork にした理由そのもの。
- core の `save` / `into-nu` / `into-df` / `into-lazy` / `collect` / `schema` / `columns` /
  `shape` / `summary` / `profile` / `into-repr` / `cache`(polars の plan cache で、
  handle の表とは別物)。
- `polars query`(sqlparser 依存の SQL 実行)。
- 本家のソース内テスト(`#[cfg(test)]`)。移す手間は差分を増やすだけ。fork で新しく書く
  テストだけ `tests/` に置く。

## seekzstdsep を圧縮層として挟む

polars は csv / ndjson の圧縮ファイルを全体展開してからしか読めない(seek 不可、展開は単一
スレッド、slice の pushdown は展開後)。fork の registry でこれを埋める。seekable zstd の frame を
単位に読む層を 1 つ作り、その上に csv / ndjson のパーサを載せる。

**この層は plugin 本体ではなく `seekzstdsep-scan` crate に置く**(`nu-polars-dyn-build` で組み込む)。
plugin が publish するバイナリは polars 自身が読む 4 形式だけを持ち、`seekzstdsep` に依存しない。

接尾辞は `.seek.zst`。seekzstdsep は元の名前を残して `<元の名前>.seek.zst` を作るので
(`seekzstdsep compress events.jsonl` → `events.jsonl.seek.zst`)、`.csv.seek.zst` /
`.ndjson.seek.zst` が「接尾辞の最長一致でパーサを選ぶ」registry の設計とそのまま噛み合う。
素の `.zst`(seek 不可、polars が全体展開)は登録しない。`polars_dyn open x.csv.zst` は
登録名の列挙エラーになる。

1. **frame 層は `seekzstdsep-scan` crate に置く。** frame ごとの読み出し、rayon の frame 並列、
   chunk の連結を持つ。パーサは「1 frame の `&[u8]` と frame 番号と schema → `DataFrame`」と
   「schema」の 2 関数を注入する(frame 境界 = レコード境界なので、パーサは frame をまたぐ
   状態を持たない)。frame は `seekzstdsep` の `RecordReader` でレコード番号から読む。
   `seekzstdsep` は crates.io から exact semver で依存する。

   `n_rows` は frame 範囲に写像しない。frame が何レコード持つかは読むまで分からないので、
   frame のバッチ単位で読みながら行数が足りたところで打ち切る。`n_rows` はファイルの行に対する
   指定なので、predicate より**先**に当てる(`slice 0 26 | filter ...` は「先頭 26 行のうち
   条件に合うもの」)。

   **predicate pushdown と projection pushdown は取る。**`n_rows` が無いときは predicate を
   frame ごとの `DataFrame` に当てるので、全件を連結してから絞るのに比べてピークが小さい
   (`n_rows` があるときは先に行を確定する必要があるので連結後に当てる)。ただし polars が押し込む式には
   optimizer の内部表現にしか存在しないもの(`sort` + `slice` の動的 top-k)があり、これを
   `LazyFrame::filter` に渡すとプロセスが落ちる。当てずに素通しする — その式は速くするための
   境界で、答えの一部ではない(`Sort` が自分の slice を保持している)。
2. **`.csv.seek.zst` / `.ndjson.seek.zst` を同じ crate の `ScanSource` にする。**
   plain の ndjson が `.ndjson` と `.jsonl` を持つのに合わせ、`.jsonl.seek.zst` も同じ source に
   登録する。
   パーサは polars 標準の reader を `Cursor` に当てるだけ。`polars_dyn open` のコマンドは
   変えない。

   `--opts` も形式の option struct をトップレベルに置く形のままだが、**受ける集合は狭い**。
   frame ごとに reader を作るので、ファイル全体を主語にするオプションは frame ごとに効いて
   意味が変わる。次の 8 つはエラーにする(`polars_dyn slice` / `select` / `rename` が代わり、
   行に番号を振るものには代わりが無い):

   - 行を取り出す: `n_rows` / `skip_rows` / `skip_lines` / `skip_rows_after_header`
   - 行に番号を振る: `row_index`
   - 列を取り出す: `columns` / `projection`
   - 列に名前を付ける: `column_names_overwrite`

   `scan` キー(`UnifiedScanArgs`)も受けない。中身の大半は polars 自身の file scan のもので
   anonymous scan には届かず、届くもの(`pre_slice` → `n_rows`、`projection` → 列)は
   クエリ側が埋めるため。

   残りは素通しするが、`infer_schema_length` は schema を決める frame 0 までしか見ない
   (plain との差。詳細は `seekzstdsep-scan/src/seek_zst.rs` のモジュール doc)。

採らなかった案: 汎用の frame 層を `seekzstdsep` の隣の crate として publish し、logfmt と
fork の両方から使う。`seekzstdsep-scan` が `pub` で持つので、logfmt 側が要るならそこを依存に
足せばよく、置き場所をもう 1 つ作る理由が無い。

**非圧縮ファイルは frame 層で扱わない。** 素の `.csv` / `.ndjson` は polars の標準 scan が
multi-threaded reader と projection / predicate pushdown 込みで読めるので、改行で切った固定長
window を frame と見なす入力実装を足しても、polars より遅いものを 1 本増やすだけになる。
logfmt は事情が違い(polars に logfmt reader が無い)、非圧縮の分割は polars-logfmt が
自前で持っている。

## 作業の進め方

- 作業はこの repo をカレントにした別プロジェクト(別セッション)で行う。
  `~/bot` の規約や memory を持ち込まない。
- fork の仕様は**すべて**この `docs.dev/` に置く(合意事項・設計・ABI 契約など)。
  `docs/` は利用者向けに取っておき、開発中の仕様は入れない。
- ビルドの `CARGO_TARGET_DIR` はローカルディスクに置く(この repo は NFS 上)。

## 未決の論点

実装に入る範囲では無し。実装時に確認する事項は各節に「実装時に確認」「未検証」として残している。
