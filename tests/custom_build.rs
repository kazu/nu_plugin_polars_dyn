//! `nu-polars-dyn-build` builds a plugin binary with a scan source crate compiled in, and that
//! binary reads the crate's sources through `polars_dyn open`. Runs the `nu` on `PATH` against
//! the binary it just built.

mod common;

use std::{fs, path::Path, process::Command};

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
        format!(
            "[package]\nname = \"from_crates_io\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
             publish = false\n\n[dependencies]\nnu_plugin_polars_dyn = \"{}\"\n",
            env!("CARGO_PKG_VERSION")
        ),
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
    let plugin = common::plugin_with(
        &common::plugin_dir("rows_scan"),
        &[("rows_scan", Path::new("tests/rows_scan"))],
    );

    let rows = out.path().join("x.rows");
    fs::write(&rows, "a\nb\nc\n").expect("write fixture");
    assert_eq!(
        collect_as_nuon(&plugin, &rows, "").trim(),
        "[[line]; [a], [b], [c]]"
    );
    assert_eq!(
        collect_as_nuon(&plugin, &rows, "--opts {rows: {skip: 1}}").trim(),
        "[[line]; [b], [c]]"
    );

    let csv = out.path().join("x.csv");
    fs::write(&csv, "a,b\n1,2\n").expect("write fixture");
    assert_eq!(
        collect_as_nuon(&plugin, &csv, "").trim(),
        "[[a, b]; [1, 2]]",
        "the built-in sources are still registered"
    );

    a_url_does_not_reach_a_compiled_in_source(&plugin);
    a_crate_taking_the_plugin_from_crates_io_builds(out.path());
    a_crate_without_the_entry_point_fails_to_compile(out.path());
}

/// A compiled-in source is handed bytes that the head of the chain opened, and this binary has no
/// source that opens `ssh://`. Part of the test above to reuse the binary it built.
fn a_url_does_not_reach_a_compiled_in_source(plugin: &Path) {
    let output = Command::new("nu")
        .args([
            "--no-config-file",
            "--plugins",
            &format!("[{}]", plugin.display()),
            "-c",
            "polars_dyn open ssh://u@h/data.rows",
        ])
        .output()
        .expect("`nu` must be on PATH to run the integration tests");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "a URL was opened:\n{stderr}");
    assert!(
        stderr.contains("No scan source for scheme `ssh`")
            && stderr.contains("registered schemes: file"),
        "stderr:\n{stderr}"
    );
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

    let output = common::build_plugin(out, &[("no_entry", &bad)]);
    assert!(!output.status.success(), "the build should have failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("scan_sources"),
        "the compile error should name the missing entry point\nstderr:\n{stderr}"
    );
}
