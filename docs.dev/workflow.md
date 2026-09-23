# 作業の進め方(git_task)

実装が一通り終わるまで、作業は `git_task` で回す。task 1 件 = branch 1 本で、**直列に 1 件ずつ**
進め、実装 → CI → review → merge の記録を task ディレクトリに残す。何を作るかは
[agreements.md](./agreements.md)、書き方は [rules.md](./rules.md) と
[rust_style.md](./rust_style.md)。この文書は**手順だけ**。

`git_task` は kazu の nu tool(`~/.config/buddy-space/agent/tools/scripts/git_task.nu`)。
task の実体は repo の外 `~/.local/share/git_task/nu_plugin_polars_dyn/<state>/<id>/` にある
(store 名は cwd の git repo のディレクトリ名で決まる。worktree の中から叩いても同じ store)。

## 呼び方

nu から `source` して叩く(`use` は module 名と同名の def があるので不可)。この repo では
nu_run(MCP)か `nu -c` のどちらでも同じ 1 行:

```nu
cd /mnt/bcachefs/xtakei/host/all/git/github.com/kazu/nu_plugin_polars_dyn
source ~/.config/buddy-space/agent/tools/scripts/git_task.nu
git_task ls | get results
```

- 戻り値は nu の record / table。文字列を目視で拾わず `where` / `get` / `open` で取る。
- `id` は 3 桁 zero-pad の文字列(`"001"`)。`git_task ls` で確認してからそのまま渡す。
- subcommand: `new` / `add_comment` / `ls` / `summary` / `view` / `cat` / `path` / `trans` /
  `make_pr` / `ci` / `merge`。

## 状態と担当

`backlog → ready → in_process → in_review → out_review → done`。前進は隣接遷移だけが
デフォルトで許され、飛ばす・戻すには `--allow-nonlinear` が要る。

| 遷移 | 誰が |
|---|---|
| `new`(→ backlog)、backlog → ready | kazu。agreements.md の順序から task 本文を起こすのは AI がやってよいが、`git_task new` を打つのは kazu が指示したときだけ |
| ready → in_process | 実装する AI |
| in_process → in_review | 実装する AI(ゲート緑 + commit 済み) |
| in_review → out_review | review する AI(実装した AI とは別の subagent) |
| out_review → done、`merge` | **kazu だけ**。`--confirm` 無しでは tool が拒む。AI は呼ばない。merge は `main` を checkout した状態で `git_task merge <id> --confirm`(`020_pull_request.txt` が merge commit の message になる) |

task は agreements.md の順序で起票済みで、依存は各 task のコメントにある。依存先が `done`
になるまで次の task を `ready` にしない。

## 実装の手順

1. **拾う**: `git_task ls | get results | where status == "ready"` から 1 件。無ければ待つ。
   `git_task view <id> 000_task.md | get detail` で本文と受け入れ条件を読む。
2. **着手**: `git_task trans <id> --to in_process`。
3. **branch**: **worktree は作らず、この checkout で直接作業する**(例外。task は直列に
   1 件ずつ進めるので並行する branch が無く、polars のフルビルドを worktree ごとに持つ
   コストが要らない)。`main` から `feature/<slug>` を切って checkout する:
   ```nu
   ^git switch -c feature/<slug> main
   ```
   `CARGO_TARGET_DIR` は 1 つ、ローカルディスク上(この repo は NFS 上)。task の途中で別の
   task に手を出さない。前の task が `done` になるまで次の branch を切らない。
4. **実装**: agreements.md に無い設計を入れない。判断が要る箇所(流用か新規か、名前)は
   書く前に列挙して kazu に出す。新しいテストは `tests/` に。
5. **ゲート**: `nu -c "use toolkit.nu; toolkit check"`(fmt --check、clippy -D warnings、test)。
   commit ごとに fmt。
6. **commit**: kernel 流 `subsystem: 命令形のサマリ`。1 bug = 1 commit、リファクタは別 commit。
   push 済みの commit は amend しない。
7. **PR 文面と CI を記録**:
   ```nu
   git_task make_pr <id> --target main --branch feature/<slug> --url /mnt/bcachefs/xtakei/host/all/git/github.com/kazu/nu_plugin_polars_dyn
   git_task ci <id>
   ```
   `make_pr` は `git request-pull` の本文を `020_pull_request.txt` に、branch 情報を
   `010_branch.nuon` に書く。`--worktree` のデフォルトはこの checkout。`--url` のデフォルトは
   remote `gh` なので、GitHub に repo を作って `gh` を張るまでは repo のパスを渡す。`ci` は
   この checkout で `make ci` を走らせ、戻りの `exit_code` が 0 でなければ先に進まない
   (`030_ci_result.log` に step ごとの出力)。`git_task ci` は並走させない。
8. **review に出す**: `git_task trans <id> --to in_review`。
9. **review 対応**: `out_review` になるまで離れない。`git_task summary <id>` で file 一覧、
   序数最大の `1NN_comment.md` を `git_task view` で読み、指摘を直す → ゲート → commit →
   `git_task ci` → 対応内容を `git_task add_comment <id> --f <md>` で記録。**直したら review を
   やり直してもらう。**部分対応で「完了」と言わない。`in_process` に戻されたら直して再提出。

## review の手順(review する AI)

1. `git_task ls | get results | where status == "in_review"` から 1 件。
2. `010_branch.nuon` を `open` して `{base, branch, worktree}` を取り、**実 diff を一次情報で**見る:
   `^git -C <worktree> diff <base>...<branch>`(worktree はこの checkout)。実装側の要約で
   判断しない。
3. `git_task ci <id>` を自分で走らせ `exit_code == 0` を確認する。前回の緑を流用しない。
4. 判定基準は複製せず、次を毎回開いて当てる: `000_task.md` の受け入れ条件、
   agreements.md(設計に無いものが入っていないか、採らなかった案が復活していないか)、
   rules.md(YAGNI、コメント、ガード、ワークアラウンドの分離)、rust_style.md
   (unwrap、`SAFETY`、macro の条件、消さないコードに触っていないか、テストの置き場)。
5. 結果は**二値**(NG の件数か、NG ゼロ)。NG は全件、行番号と引用付きで
   `git_task add_comment <id> --f <md>` に書く。「残課題」「将来検討」を混ぜない。
6. NG ゼロ + CI 緑を自分で確認できたときだけ `git_task trans <id> --to out_review`。
   NG があれば `in_review` のまま実装側の対応を待ち、対応後は 2 から全部やり直す。
   実装側に明示的に返すときだけ `git_task trans <id> --to in_process --allow-nonlinear`。

## `make ci` の契約

`git_task ci` は worktree で `make ci BASE_REF=<base> CI_LOG=<partial> CI_MAX_FILE=<f>` を叩く。
この repo の `Makefile` は次を満たす:

- `__git_task_ci_probe` target がある(`git_task ci` が事前に `make -q` で存在を確かめる)。
- `ci` は toolkit.nu の `fmt --check` / `clippy` / `test` を **fail-fast 無しで全部**走らせ、
  step ごとに `===== NAME (exit N) =====` と出力を `CI_LOG` に追記し、exit は各 step の最大値。
  `CI_MAX_FILE` が渡されればその値を書く。
- ゲートの定義は toolkit.nu の 1 箇所。Makefile はそれを呼ぶだけで、cargo のコマンドを
  二重に書かない。

## リリース

clean な `main` で `make release version=MAJOR.MINOR.PATCH`。toolkit.nu の `release` が
`Cargo.toml` の version、`seekzstdsep-scan` の pin、`docs/custom_build.md` の例を書き換え、
`Cargo.lock` を更新し、`check` を通してから commit し、tag `<version>`(`v` 無し)を打って
`gh` に `main` と tag を push する。branch は切らない。crates.io には publish しない。
tag が要るのは `nu-polars-dyn-build` が自分の版の tag を `nu_plugin_polars_dyn` の git 依存に
使うため。

## やらないこと

- `git_task trans <id> --to done` と `git_task merge`(kazu だけ)。
- `main` へ直接 commit / push。
- `git_task ci` の `exit_code` を見ずに進む。
- task の scope を勝手に切る・広げる。scope の変更は kazu に聞く。
- id を勘で打つ(`1` と `"001"` は別物)。
