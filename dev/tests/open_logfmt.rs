//! `polars_dyn open` on the dev bin reads `.logfmt` and `.logfmt.zst` through the `logfmt`
//! source. Runs the `nu` on `PATH` against the built dev bin.

use std::{
    fs::File,
    process::{Command, Output},
};

const LINES: &str = "level=info msg=start n=1\nlevel=error msg=boom n=2\nlevel=info msg=stop n=3\n";

fn nu(script: &str) -> Output {
    let plugin = env!("CARGO_BIN_EXE_nu_plugin_polars_dyn_dev");
    Command::new("nu")
        .args([
            "--no-config-file",
            "--plugins",
            &format!("[{plugin}]"),
            "-c",
            script,
        ])
        .output()
        .expect("`nu` must be on PATH to run the integration tests")
}

fn run_nu(script: &str) -> String {
    let output = nu(script);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "nu failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    stdout
}

fn fail_nu(script: &str) -> String {
    let output = nu(script);
    assert!(!output.status.success(), "nu succeeded unexpectedly");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn collect_as_nuon(path: &str, opts: &str) -> String {
    run_nu(&format!(
        "polars_dyn open {path} {opts} | polars_dyn collect | polars_dyn into-nu | to nuon"
    ))
}

#[test]
fn opens_logfmt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("app.logfmt");
    std::fs::write(&path, LINES).expect("write fixture");
    let path = path.display().to_string();

    assert_eq!(
        collect_as_nuon(&path, "").trim(),
        "[[level, msg, n]; [info, start, 1], [error, boom, 2], [info, stop, 3]]"
    );
    assert_eq!(
        collect_as_nuon(&path, "--opts {line_filter: 'level=error'}").trim(),
        "[[level, msg, n]; [error, boom, 2]]"
    );
}

#[test]
fn opens_logfmt_zst() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("app.logfmt.zst");
    let mut out = File::create(&path).expect("create fixture");
    seekzstdsep::convert_text_to_seekable_zst_reader(LINES.as_bytes(), &mut out, 65536, b"\n")
        .expect("seekable zst fixture");
    drop(out);

    assert_eq!(
        collect_as_nuon(&path.display().to_string(), "").trim(),
        "[[level, msg, n]; [info, start, 1], [error, boom, 2], [info, stop, 3]]"
    );
}

#[test]
fn rejects_unknown_opt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("app.logfmt");
    std::fs::write(&path, LINES).expect("write fixture");
    let stderr = fail_nu(&format!(
        "polars_dyn open {} --opts {{line_filtre: x}}",
        path.display()
    ));
    assert!(stderr.contains("line_filtre"), "stderr:\n{stderr}");
}
