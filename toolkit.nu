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

# version を上げる commit を main に置き、tag を打って push する。`make release version=0.2.0` の中身。
# clean な main の上で Cargo.toml と seekzstdsep-scan の pin と docs/custom_build.md の例を
# 書き換え、Cargo.lock を更新し、check を通してから commit する。
# tag は `v` 無しの `<version>`。nu-polars-dyn-build がその tag を git 依存に使うので push まで行う。
export def release [
    version: string  # 例: 0.2.0
    --remote: string = "gh"  # push 先
] {
    if $version !~ '^\d+\.\d+\.\d+$' {
        error make { msg: $"release: version must be MAJOR.MINOR.PATCH, got `($version)`" }
    }
    if (^git status --porcelain | str trim | is-not-empty) {
        error make { msg: "release: the working tree is not clean" }
    }
    if (^git branch --show-current | str trim) != "main" {
        error make { msg: "release: run it on main" }
    }
    if (^git tag --list $version | str trim | is-not-empty) {
        error make { msg: $"release: tag `($version)` already exists" }
    }
    let minor = ($version | split row "." | first 2 | str join ".")

    open --raw Cargo.toml
    | str replace --regex '(?m)^version = "[^"]+"' $'version = "($version)"'
    | save --force Cargo.toml
    open --raw seekzstdsep-scan/Cargo.toml
    | str replace --regex 'nu_plugin_polars_dyn = \{ version = "=[^"]+"' $'nu_plugin_polars_dyn = { version = "=($version)"'
    | save --force seekzstdsep-scan/Cargo.toml
    open --raw docs/custom_build.md
    | str replace --regex '(?m)^nu_plugin_polars_dyn = "[^"]+"' $'nu_plugin_polars_dyn = "($minor)"'
    | save --force docs/custom_build.md
    ^cargo update --workspace --offline
    check
    ^git add Cargo.toml Cargo.lock seekzstdsep-scan/Cargo.toml docs/custom_build.md
    ^git commit -m $"release: bump the version to ($version)"
    ^git tag $version
    ^git push $remote main
    ^git push $remote $version
}
