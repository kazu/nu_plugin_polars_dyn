# AGENTS.md

nushell の polars plugin の fork。Python 無しで polars をシェルとして使う。

## 最初に読む

1. `docs.dev/agreements.md` — 設計の合意。ここに無い設計を実装しない。設計を変えるなら先に
   この文書を変える
2. `docs.dev/rules.md` — 作業の規約(YAGNI、コメント、ガード、ワークアラウンド)
3. `docs.dev/rust_style.md` — Rust の書き方(本家との違いを含む)
4. `docs.dev/workflow.md` — 作業の進め方。task は `git_task` で直列に 1 件ずつ回す(拾う →
   この checkout で branch → 実装 → ゲート → `make_pr` / `ci` → review)。worktree は作らない。
   `done` と `merge` は kazu だけ

`docs/` は利用者向け。開発中の仕様は `docs.dev/` に置く。

## ビルドとゲート

- `CARGO_TARGET_DIR` はローカルディスクに置く(この repo は NFS 上。NFS 上でビルドすると壊れる)
- commit 前: `nu -c "use toolkit.nu; toolkit fmt"` と `nu -c "use toolkit.nu; toolkit clippy"`
- 終わったと言う前: `nu -c "use toolkit.nu; toolkit check"`(fmt、clippy -D warnings、test)。
  `git_task ci` は同じものを `make ci` 経由で走らせる
- `cargo` を並列に走らせない。`cargo clean` をしない

## 守ること

- `.unwrap()` 禁止(`unwrap_used = deny`)。ユーザ入力や外部システム起因で panic しない
- `unsafe` には `// SAFETY:` を付ける
- 依存は exact semver。版の無い git / path 依存は publish する manifest に置かない(`tests/` の crate と、
  版を併記した path 依存は例外。`docs.dev/rust_style.md`「依存の追加」)
- 本家から引き継いだコードは触らない。差分は最小に保つ
- 新しいテストは `tests/` に置く
- commit message は kernel 流(`subsystem: 命令形のサマリ`)。conventional commits ではない
