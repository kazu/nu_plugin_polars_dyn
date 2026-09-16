# 開発用のゲート。`use toolkit.nu; toolkit check` で fmt / clippy / test を順に走らせる。
# `CARGO_TARGET_DIR` はローカルディスクに置く(この repo は NFS 上)。

# 一覧を表示する
export def main [] {
    scope commands
    | where name =~ '^toolkit ' and name != 'toolkit main'
    | select name description
    | update name { str replace --regex '^toolkit ' '' }
}

# rustfmt を掛ける。--check なら差分があれば失敗する
export def fmt [
    --check  # 整形せず、差分があれば失敗する
] {
    if $check {
        ^cargo fmt --all -- --check
    } else {
        ^cargo fmt --all
    }
}

# clippy を warning も error として走らせる
export def clippy [] {
    ^cargo clippy --workspace --all-targets -- -D warnings
}

# テストを走らせる
export def test [] {
    ^cargo test --workspace
}

# commit 前・「終わった」と言う前の全ゲート: fmt --check、clippy、test
export def check [] {
    fmt --check
    clippy
    test
}
