//! `nu-polars-dyn-build` builds a plugin binary with a scan source crate compiled in, and that
//! binary reads the crate's sources through `polars_dyn open`. Runs the `nu` on `PATH` against
//! the binary it just built.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const BUILD: &str = env!("CARGO_BIN_EXE_nu-polars-dyn-build");

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The target directory of this test run. The generated project builds into it so the
/// dependencies it shares with the plugin are already there.
fn target_dir() -> PathBuf {
    Path::new(BUILD)
        .parent()
        .and_then(Path::parent)
        .expect("the test binary lives in <target>/<profile>")
        .to_path_buf()
}

/// Builds a plugin with `name` taken from `crate_dir` compiled in, placing it in `out`.
///
/// `crate_dir` is given as the caller wrote it — the test passes a relative one, which only
/// works if the builder resolves it against the working directory rather than against the
/// manifest it generates somewhere else.
fn build_plugin(out: &Path, name: &str, crate_dir: &Path) -> Output {
    Command::new(BUILD)
        .args([
            name,
            "--path",
            &format!("{name}={}", crate_dir.display()),
            "--out",
            &out.display().to_string(),
            "--debug",
        ])
        .env("NU_POLARS_DYN_SOURCE", repo())
        .env("CARGO_TARGET_DIR", target_dir())
        .output()
        .expect("nu-polars-dyn-build must run")
}

fn run_nu(plugin: &Path, script: &str) -> String {
    let output = Command::new("nu")
        .args([
            "--no-config-file",
            "--plugins",
            &format!("[{}]", plugin.display()),
            "-c",
            script,
        ])
        .output()
        .expect("`nu` must be on PATH to run the integration tests");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "nu failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    stdout
}

fn collect_as_nuon(plugin: &Path, source: &Path, opts: &str) -> String {
    run_nu(
        plugin,
        &format!(
            "polars_dyn open {} {opts} | polars_dyn collect | polars_dyn into-nu | to nuon",
            source.display()
        ),
    )
}

/// A crate that takes `nu_plugin_polars_dyn` from crates.io, as one is written once the plugin
/// is published. It has to end up on the same copy as the generated project, or its
/// `ScanSource` is a different trait.
fn write_crate_taking_the_plugin_from_crates_io(dir: &Path) {
    fs::create_dir_all(dir.join("src")).expect("create the crate directory");
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"from_crates_io\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
         publish = false\n\n[dependencies]\nnu_plugin_polars_dyn = \"0.1\"\n",
    )
    .expect("write the manifest");
    fs::write(
        dir.join("src/lib.rs"),
        "use nu_plugin_polars::scan::ScanSource;\n\
         pub fn scan_sources() -> &'static [&'static dyn ScanSource] {\n    &[]\n}\n",
    )
    .expect("write the crate");
}

/// A crate that compiles but does not expose `scan_sources()`.
fn write_crate_without_the_entry_point(dir: &Path) {
    fs::create_dir_all(dir.join("src")).expect("create the crate directory");
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"no_entry\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
         publish = false\n\n[dependencies]\n",
    )
    .expect("write the manifest");
    fs::write(dir.join("src/lib.rs"), "pub fn other() {}\n").expect("write the crate");
}

#[test]
fn builds_a_plugin_with_a_scan_source_compiled_in() {
    let out = tempfile::tempdir().expect("tempdir");
    let output = build_plugin(out.path(), "rows_scan", Path::new("tests/rows_scan"));
    assert!(
        output.status.success(),
        "nu-polars-dyn-build failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let plugin = out.path().join("nu_plugin_polars_dyn");
    assert!(plugin.is_file(), "the plugin was not placed in --out");

    let rows = out.path().join("x.rows");
    fs::write(&rows, "a\nb\nc\n").expect("write fixture");
    assert_eq!(
        collect_as_nuon(&plugin, &rows, "").trim(),
        "[[line]; [a], [b], [c]]"
    );
    assert_eq!(
        collect_as_nuon(&plugin, &rows, "--opts {skip: 1}").trim(),
        "[[line]; [b], [c]]"
    );

    let csv = out.path().join("x.csv");
    fs::write(&csv, "a,b\n1,2\n").expect("write fixture");
    assert_eq!(
        collect_as_nuon(&plugin, &csv, "").trim(),
        "[[a, b]; [1, 2]]",
        "the built-in sources are still registered"
    );

    a_crate_taking_the_plugin_from_crates_io_builds(out.path());
    a_crate_without_the_entry_point_fails_to_compile(out.path());
}

/// Part of the test above for the same reason as the one below.
fn a_crate_taking_the_plugin_from_crates_io_builds(out: &Path) {
    let dir = out.join("from_crates_io");
    write_crate_taking_the_plugin_from_crates_io(&dir);

    let output = common::build_plugin(out, &[("from_crates_io", &dir)]);
    assert!(
        output.status.success(),
        "a crate taking the plugin from crates.io must reach the same copy\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Part of the test above rather than its own, so that only one `cargo` runs at a time and the
/// second build reuses what the first one left in the target directory.
fn a_crate_without_the_entry_point_fails_to_compile(out: &Path) {
    let bad = out.join("no_entry");
    write_crate_without_the_entry_point(&bad);

    let output = build_plugin(out, "no_entry", &bad);
    assert!(!output.status.success(), "the build should have failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("scan_sources"),
        "the compile error should name the missing entry point\nstderr:\n{stderr}"
    );
}
