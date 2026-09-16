# 合意事項

設計の議論で決まったことだけを書く。未決の論点は末尾に分けて置く。

## 目的

nushell から、Rust 製の polars 拡張(動的ライブラリ `.so` / `.dll`)を FFI 経由で
自由に使えるようにする。polars のオブジェクトは遅延評価(LazyFrame)のまま
パイプラインに流す。

## 名前

- crate 名・バイナリ名・nu 側のコマンド接頭辞をすべて `polars_dyn` に揃える
  (バイナリは `nu_plugin_polars_dyn`、コマンドは `polars_dyn scan` など)。
- 本家 `nu_plugin_polars` とは**完全に別の plugin**として同居できるようにする。
  コマンド接頭辞 `polars` は使わない。短く打ちたい場合は nu 側の alias で解く。

## 出自

- nushell tag `0.114.1` の `crates/nu_plugin_polars` を `git subtree split` で
  切り出した履歴(297 commit)を `main` の起点にする。切り出し元の blame が追える。
- 本家(nushell / nu-polars)への働きかけはしない。解決はこの repo の中で完結させる。

## 設計の方針

- 本家 `polars open` にあるファイル形式ごとの分岐と個別パースは使わない。
  パスをそのまま polars 本体(または `.so` 側の IO)に渡して LazyFrame を返す
  抽象化コマンドを 1 つ新設する(仮称 `polars_dyn scan`)。
- polars のオブジェクトは nushell の `CustomValue` で不透明な型として流す。
  LazyFrame / Expr / Series(eager)のどれでも内包できる enum のコンテナを 1 つ定義する。
  ポインタの生操作はしない。
- 外部の `.so` は、対話セッション中ではなく **plugin プロセスの起動時**に
  Rust コード側から(`libloading` 等で)ロードする。nushell のパーサやコマンド登録の
  制約を回避し、別 plugin 間で `CustomValue` を渡せない問題を根本から避ける。
- `.so` との受け渡しは nushell の型を挟まず polars のオブジェクトを直接渡す
  (Arrow C Data Interface 等)。

## 作業の進め方

- 作業はこの repo をカレントにした別プロジェクト(別セッション)で行う。
  `~/bot` の規約や memory を持ち込まない。
- fork の仕様は**すべて**この `docs.dev/` に置く(合意事項・設計・ABI 契約など)。
  `docs/` は利用者向けに取っておき、開発中の仕様は入れない。
- ビルドの `CARGO_TARGET_DIR` はローカルディスクに置く(この repo は NFS 上)。

## 未決の論点

1. `.so` 境界の ABI。Rust ABI(同一 toolchain・同一 polars 版が前提)か、
   Arrow C stream + serde 直列化の自前契約か。`AnonymousScan` の pushdown 引数
   (`with_columns` / `n_rows` / `predicate: Expr`)をどう渡すかがここで決まる。
2. `polars_dyn scan` の引数の形(パス丸投げと `.so` の対応関係)。
3. `CustomValue` コンテナ enum の中身(LazyFrame / Expr / Series / DataFrame の扱い)。
4. `.so` の探索場所と登録タイミング(plugin 起動時に何を読むか)。
5. 本家から切り出したコードのうち消すもの・残すもの。
6. polars のバージョン(本家は `=0.54.4` 固定。fork で nu のリリースから切り離せる)。
