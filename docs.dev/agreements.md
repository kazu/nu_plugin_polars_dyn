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
- registry の境界の入力は source のバイト列(`ReadAt`)とオプションの bytes に保つ。`--opts` の
  中身を nu 側で覗かない、という一点だけが理由で、FFI を越えるためではない。文字列を開くのは
  plugin 側の仕事(「`polars_dyn open` と registry」の節)。

## `polars_dyn open` と registry

```
polars_dyn open <source: string> [--format (-f) <a,b,c>] [--opts (-o) <record>]  → LazyFrame
```

source 文字列から **scan の列(chain)** を組み、scan どうしを `ReadAt`(offset 指定で読める
バイト列)で繋ぐ。plugin は個々の scan が何と繋がるかを知らない。この API で作った scan は
何と何でも繋がる(`ssh → seek-zst → ndjson`、`file → seek-zst → csv`、`file → parquet`)。

- registry の境界は trait 1 つ。返り値は LazyFrame で、collect はしない。

  ```rust
  pub trait ReadAt: Send + Sync {
      fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
      fn len(&self) -> io::Result<u64>;   // seek.zst が末尾の seek table を読むのに要る
  }

  pub trait ScanSource: Send + Sync {
      fn name(&self) -> &'static str;                         // "seek-zst"
      fn suffixes(&self) -> &'static [&'static str];          // [".seek.zst"]
      /// chain の先頭として開く URL scheme。デフォルト空。
      fn schemes(&self) -> &'static [&'static str] { &[] }    // ["ssh"]
      /// scheme で選ばれたとき、bytes 無しで呼ばれる。デフォルトはエラー。
      fn open(&self, url: &str, opts: &[u8]) -> PolarsResult<Arc<dyn ReadAt>>;
      /// chain の途中で呼ばれる: bytes を受けて bytes を返す。デフォルトはエラー。
      fn wrap(&self, source: Arc<dyn ReadAt>, opts: &[u8]) -> PolarsResult<Arc<dyn ReadAt>>;
      /// chain の末尾で呼ばれる: bytes を frame にする。デフォルトはエラー。
      fn scan(&self, source: Arc<dyn ReadAt>, opts: &[u8]) -> PolarsResult<LazyFrame>;
  }
  ```

  3 つの動作すべてにデフォルトがあるので、scan は自分がやることだけを書く(`seek-zst` は
  `wrap` だけ、`ssh` は `open` だけ、`ndjson` は `scan` だけ)。016 の impl(`scan` だけ)は
  無変更でコンパイルが通る。`ReadAt` は `&self` で読むので `Arc` で共有するだけで並列に使え、
  S3 の range GET にそのまま写る。`File` の実装は unix の `FileExt::read_at`、Windows の
  `FileExt::seek_read`。`Read + Seek` を要求する下流ライブラリには `scan::ReadAtCursor`
  (`Arc<dyn ReadAt>` + 位置。thread ごとに 1 つ)でアダプトする。
  参考にしたのは polars-logfmt の `SeekableVfsFile`(文字列の解決と読み口を分ける)だが、
  `Read + Seek` + `clone_handle` ではなく offset 指定の読みを境界にした。

- **plugin の手順(これ以外を知らない)**:
  1. source に `<scheme>://` があればその scheme を持つ scan を先頭に、無ければ built-in の `file` を
     先頭にする(scheme 無しのパスは engine のカレントディレクトリ基準で絶対パスにしてから渡す)。
     scheme を持つ scan が無ければ登録 scheme を列挙してエラー
     (`No scan source for scheme `ftp``、`registered schemes: file, ssh`)。
  2. 残りの文字列の**末尾から**接尾辞を剥く。登録された接尾辞のうち最長一致を 1 つ取り、その scan を
     列に足し、接尾辞を落とす。一致しなくなるまで繰り返す。
     `ssh://h/log/x.jsonl.seek.zst` → `ssh`, `seek-zst`, `ndjson`。`x.parquet` → `file`, `parquet`。
     1 つも剥けなければ、接尾辞を持つ scan の名前を列挙してエラー(`No scan source for `./x.unknown``、
     `registered: parquet, csv, ipc, ndjson, ...`)。接尾辞は glob を展開する前の文字列から剥く
     (`data/*.jsonl` → `file`, `ndjson`)。
  3. scheme 無しのパスは、1 の絶対パスに glob のメタ文字(`*` `?` `[`)があればローカルの
     ファイルシステムで展開する(カレントディレクトリの部分も含めて見る。本家と同じ)。規則は polars の path scan(本家の `polars open`)と同じにする:
     同じ `glob` crate で展開し、ディレクトリと空のファイルを除き、パスを文字列の順に並べる。
     0 件はエラー(``No file matches `data/*.jsonl` ``)。メタ文字が無ければ今と同じ 1 件。
     scheme 付きの source は展開しない。
     `AnonymousScan` には polars がパスの一覧を渡す欄が無いので、複数ファイルは plugin が持つ。
  4. 3 の 1 件ずつに、先頭の `open(url)`、途中の `wrap(bytes)`、末尾の `scan(bytes)` を呼ぶ。
     末尾が `scan` を持たなければその scan のデフォルトのエラー(`./x.seek.zst` は `seek-zst` が
     frame を返せない)。得た LazyFrame は polars の `concat`(縦、デフォルトの `UnionArgs`)で
     1 つにし、1 件ならそのまま返す。列の揃わないファイルは collect のときに `concat` のエラーになる。
     scan は今と同じく 1 回に 1 ファイルの bytes を受けるだけで、glob も前後も知らない。
  5. `--format a,b,c` は 2 の結果を置き換える(scheme は文字列から)。知らない名前は登録名を
     列挙してエラー。
  6. `--opts` は **scan 名をキーにした record**(`--opts {csv: {has_header: false}, ssh: {port: 2222}}`)。
     名前ごとに JSON bytes にして各 scan に渡し、省略された scan には空を渡す。chain に無い名前は
     エラー(`unknown scan `nope` in --opts`)。中身は scan ごとの契約で、plugin は覗かない。
     3 で展開した全ファイルに同じものを渡す。
     形式固有の flag(`--delimiter` 等)は付けない。`--eager` も付けない(`collect` で足りる)。
  7. 起動時に名前・scheme・接尾辞の重複を 1 回検査する。

- **plugin 本体が持つ scan は 5 つ**: `file`(scheme `file`。scheme 無しの既定。`File` を `ReadAt` に)
  と、parquet / csv / ipc / ndjson(接尾辞。bytes から読む)。それ以外は crate で、`nu-polars-dyn-build`
  が組み込む(`.seek.zst` は `seekzstdsep-scan`、`ssh` は `ssh-scan`、`.logfmt` は polars-logfmt の
  `logfmt-scan`)。

  | scan | 種類 | 入力 | 出力 | 置き場 |
  |---|---|---|---|---|
  | `file` | scheme `file` / scheme 無し | 無し | bytes | plugin |
  | `ssh` | scheme `ssh` | 無し | bytes(sftp) | `ssh-scan/` |
  | `s3` | scheme `s3` | 無し | bytes(range GET) | 別 task |
  | `seek-zst` | 接尾辞 `.seek.zst` | bytes | bytes(展開後) | `seekzstdsep-scan/` |
  | `csv` / `ndjson` | 接尾辞 | bytes | frame | plugin |
  | `parquet` / `ipc` | 接尾辞 | bytes | frame | plugin |
  | `logfmt` | 接尾辞 `.logfmt` | bytes | frame | polars-logfmt `logfmt-scan/` |

- built-in の読み方。polars の path scan は使わない(`ReadAt` の先が file とは限らない)。
  - **parquet / ipc** は bytes を全部読んで `ScanSources::Buffers` で polars 自身の lazy scan に渡す。
    pushdown と `collect --streaming` は path のときと同じ。`--opts` は形式の option struct を
    トップレベルに、`UnifiedScanArgs` を `scan` キーに(`polars-io` の serde 形)。
  - **csv / ndjson** は plugin の chunk 層(`scan::chunked`)で読む。バイト列を改行で chunk に切り、
    chunk ごとに polars の reader を `Cursor` に当てて並列に読み、連結する。chunk の幅は形式の
    `chunk_size` オプション(polars の「並列に読む単位」と同じ意味。デフォルトは polars の値)。
    切れ目は任意の改行で、csv の引用符の中の改行も切れ目になりうる(そこで切れると parse error。
    polars の path scan は引用を見て行頭を探していた。差として文書に書き、引用内改行のある csv は
    `chunk_size` をファイル長以上にして 1 chunk で読む)。schema は chunk 0 から決める。chunk ごとに reader を作るので、ファイル全体を主語にする
    オプションは frame ごとに効いて意味が変わる。次の 8 つはエラーにする(`polars_dyn slice` /
    `select` / `rename` が代わり、行に番号を振るものには代わりが無い):
    行を取り出す `n_rows` / `skip_rows` / `skip_lines` / `skip_rows_after_header`、
    行に番号を振る `row_index`、列を取り出す `columns` / `projection`、列に名前を付ける
    `column_names_overwrite`。`scan` キー(`UnifiedScanArgs`)も受けない。
    `AnonymousScan` なので `collect --streaming` は使えず、offset 付きの `slice` は届かない。
    `n_rows` は chunk 範囲に写像せず、chunk のバッチ単位で読みながら行数が足りたところで打ち切る。
    predicate と projection の pushdown は取る(`n_rows` が無いときは chunk ごとに当てる)。
    `sort` + `slice` の動的 top-k は当てずに素通しする(答えの一部ではない)。
- 採らなかった案: source ごとの named flag(`open` の分岐を作り直すことになる)、
  `inventory` crate による自動収集(構築時に渡す配列で足りる)、一覧コマンド
  (エラーメッセージの列挙で足りる)、`ReadAt` に `path()` / `chunks()` を持たせる
  (連結情報の混入。plugin が「file なら polars の path scan」と分岐することになる)、
  scan の間を `Read + Seek` や `LazyFrame` で繋ぐ(前者は `&self` で並列に読めず、後者は frame の
  上に別の frame 層を重ねられない)、層の種類ごとに trait を分ける(plugin が種類の順序を知ることに
  なり、crate が 2 種類を兼ねられない)、接尾辞を分解せず組み合わせごとに scan を 1 つ書く
  (016 まで。S3 × zst × 形式の積を書くことになる)。
- 本家の `open.rs`(形式ごとの分岐と flag 群)は**皮も残さず消す**(005 で済み)。
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
  `POLARS_ALLOW_EXTENSION` の設定、`extra` の平坦化(built-in は registry が常に持つ)、`PolarsPlugin::new` と
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
- 消すもの: insert / remove ごとの plugin GC の on / off(`set_gc_disabled` の往復)、
  `store-get`。plugin GC は起動時に 1 回 off にする(`PolarsPlugin::disable_gc_once`)。
- 表の追い出しは参照カウントと `custom_value_dropped` の drop 通知でやる。nu へ値を 1 つ返す
  たびに数えるので、`Cache::insert` は新しい id なら 1、既にある id なら加算する
  (`polars_dyn into-df` のように同じ id をもう一度返す経路がある)。同じ id を再度返すだけで
  `insert` を通らない `polars_dyn collect` は `Cache::get` で加算する。engine から drop 通知が
  来たら減らし、0 で表から消す。nu 側の clone は engine が数えていて、最後の 1 個が落ちたとき
  だけ通知が来るので、plugin 側は「返した回数」だけを数えればいい。
  採らなかった案: 表に上限 / TTL / LRU を置く(生きている handle の先が落ちる)、表そのものを
  持たない(上記のとおり handle しか渡せない)、`store-rm --all` を足して手で掃く(解放を
  利用者の作業にするだけで、忘れれば同じ)。
- 残すもの: `NuDataFrame` / `NuLazyFrame` / `NuExpression` 等の値型、nu の table と
  DataFrame の相互変換、`store-ls` / `store-rm`(drop 通知が届かない値を手で掃き出す口)。
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
  `AnonymousScan` で書かれた source(logfmt、built-in の csv / ndjson の chunk 層)は streaming で collect
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
  `version` は `0.1.0` から、`edition = "2024"`、`rust-version = "1.95.0"`、`license = "MIT"`
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
- `NuPolarsTestData` は消す(GC 往復の単体テスト専用の変種で、その往復ごと消えた)。
- 名前も変えない。**消さないコードは触らない**。残すコードは本家との差分を最小に保ち、
  上流の修正(polars 版の追従など)を cherry-pick できる状態にする。これは fork 全体の
  方針。

## 消すもの・残すもの

消す:
- `open.rs`、cache の GC 往復・`store-get`、`NuPolarsTestData`(上の各節)。
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
スレッド、slice の pushdown は展開後)。fork の registry でこれを埋める。seekable zstd を
**展開後のバイト列の `ReadAt`** にする `wrap` の scan を 1 つ作り、パーサは chain の次の scan
(built-in の csv / ndjson、logfmt-scan)に任せる。

**この層は plugin 本体ではなく `seekzstdsep-scan` crate に置く**(`nu-polars-dyn-build` で組み込む)。
plugin が publish するバイナリは polars 自身が読む 4 形式と `file` だけを持ち、zstd に依存しない。

接尾辞は `.seek.zst`、名前は `seek-zst`。seekzstdsep は元の名前を残して `<元の名前>.seek.zst` を
作るので(`seekzstdsep compress events.jsonl` → `events.jsonl.seek.zst`)、「接尾辞を末尾から剥く」
registry の設計とそのまま噛み合う(`events.jsonl.seek.zst` → `file`, `seek-zst`, `ndjson`)。
素の `.zst`(seek 不可、polars が全体展開)は登録しない。`polars_dyn open x.csv.zst` は
登録名の列挙エラーになる。

1. **`seek-zst` は `ReadAt` を受けて `ReadAt` を返す。** 展開後 offset → frame は seek table で引ける。
   実装は zeekstd の `Decoder`(`Read + Seek` の source の上で、展開後 offset に `set_offset` して
   読む。frame の途中の offset は frame 先頭から読み捨てる)。`Decoder` は thread 間で共有できないので
   `Mutex<Vec<Decoder>>` のプールから取って返す。`len` は seek table の `size_decomp`。
   下の compressed source は plugin から渡された `ReadAt` を `ReadAtCursor` で `Read + Seek` にして
   渡す(decoder ごとに cursor 1 つ、handle は `Arc` で共有)。`zeekstd` は crates.io から exact
   semver で依存する。`--opts` は取らない(空でない record はエラー)。
2. **パーサは持たない。** csv / ndjson は plugin の chunk 層が読む(「`polars_dyn open` と registry」の
   節)。chunk は展開後のバイト列を改行で切るので frame 境界とは無関係で、chunk の読みは frame を
   跨いで展開する。逆に、レコード番号から frame を引く必要が無いので、frame ごとのレコード数の
   不変条件も、それを検査する `verify_frames` も要らない(016 まではあった。展開後の offset で
   読む限り、数が揃っていなくても答えは変わらない)。
3. `.logfmt.seek.zst` も同じ層で、`file`, `seek-zst`, `logfmt` の chain になる。logfmt-scan は
   `.seek.zst` を自前で持たない。

採らなかった案: 汎用の frame 層を `seekzstdsep` の隣の crate として publish し、logfmt と
fork の両方から使う。chain にすれば frame 層は 1 つの `wrap` で、パーサ側は圧縮を知らずに済む。
seekzstdsep の `RecordReader`(レコード番号で読む)を残す案は、chunk 層が offset で読む今は
使い手が無い。

**非圧縮ファイルも同じ chunk 層で読む。** 016 までは素の `.csv` / `.ndjson` を polars の path scan に
任せていたが、chain の末尾は bytes が file か ssh か zst 展開後かを知らないので、path scan は
使えない。`collect --streaming` が built-in の csv / ndjson で使えなくなるのはその代償。
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