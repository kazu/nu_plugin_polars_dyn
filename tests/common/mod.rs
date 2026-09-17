//! Building a plugin with scan source crates compiled in, for the tests that need one.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

const BUILD: &str = env!("CARGO_BIN_EXE_nu-polars-dyn-build");

/// The target directory of this test run. The generated project builds into it so the
/// dependencies it shares with the plugin are already there.
fn target_dir() -> PathBuf {
    Path::new(BUILD)
        .parent()
        .and_then(Path::parent)
        .expect("the test binary lives in <target>/<profile>")
        .to_path_buf()
}

/// Runs `nu-polars-dyn-build` for `crates`, each taken from the directory beside it, placing the
/// plugin in `out`.
///
/// The directories are passed as given — the callers pass relative ones, which only work if the
/// builder resolves them against the working directory rather than against the manifest it
/// generates somewhere else.
pub fn build_plugin(out: &Path, crates: &[(&str, &Path)]) -> Output {
    let mut command = Command::new(BUILD);
    for (name, dir) in crates {
        command.arg(name);
        command
            .arg("--path")
            .arg(format!("{name}={}", dir.display()));
    }
    command
        .arg("--out")
        .arg(out)
        .arg("--debug")
        .env("NU_POLARS_DYN_SOURCE", env!("CARGO_MANIFEST_DIR"))
        .env("CARGO_TARGET_DIR", target_dir())
        .output()
        .expect("nu-polars-dyn-build must run")
}

/// A directory under the target directory for a plugin that outlives the test that built it.
///
/// A `TempDir` kept in a `OnceLock` is never dropped, so a plugin built into one would be left
/// behind on every `cargo test`. A fixed place is overwritten instead.
pub fn plugin_dir(name: &str) -> PathBuf {
    let dir = target_dir().join("custom-plugin-tests").join(name);
    std::fs::create_dir_all(&dir).expect("create the plugin directory");
    dir
}

/// Builds a plugin with `crates` compiled in and returns the path to it, failing the test with
/// cargo's output when the build does not succeed.
pub fn plugin_with(out: &Path, crates: &[(&str, &Path)]) -> PathBuf {
    let output = build_plugin(out, crates);
    assert!(
        output.status.success(),
        "nu-polars-dyn-build failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let plugin = out.join("nu_plugin_polars_dyn");
    assert!(plugin.is_file(), "the plugin was not placed in --out");
    plugin
}
