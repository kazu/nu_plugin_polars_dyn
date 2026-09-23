//! `polars_dyn open ssh://...` reads a file over sftp as the same rows the local file gives, alone
//! (`ssh`, `ndjson`) and through the decompressor (`ssh`, `seek-zst`, `ndjson`). The sources live
//! in the `ssh_scan` and `seekzstdsep_scan` crates, so this builds a plugin with both compiled in
//! and runs the `nu` on `PATH` against it.
//!
//! It needs an sshd that the agent can log in to, and the files it writes have to be visible
//! there at the same path, so it runs against `POLARS_DYN_SSH_TARGET` (`user@host`, usually
//! `<you>@localhost`) and prints that it was skipped when the variable is not set. CI has no sshd.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use seekzstdsep::convert_to_seekable_zst_reader;

fn target() -> Option<String> {
    let target = std::env::var("POLARS_DYN_SSH_TARGET").ok()?;
    if target.is_empty() {
        return None;
    }
    Some(target)
}

/// The plugin every test here runs against, built once for the whole file.
fn plugin() -> &'static Path {
    static PLUGIN: OnceLock<PathBuf> = OnceLock::new();
    PLUGIN.get_or_init(|| {
        common::plugin_with(
            &common::plugin_dir("ssh"),
            &[
                ("ssh_scan", Path::new("ssh-scan")),
                ("seekzstdsep_scan", Path::new("seekzstdsep-scan")),
            ],
        )
    })
}

fn run_nu(script: &str) -> String {
    let output = Command::new("nu")
        .args([
            "--no-config-file",
            "--plugins",
            &format!("[{}]", plugin().display()),
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

fn collect_nuon(source: &str, opts: &str) -> String {
    run_nu(&format!(
        "polars_dyn open {source} {opts} | polars_dyn collect | polars_dyn into-nu | to nuon"
    ))
    .trim()
    .to_owned()
}

/// Writes 40 rows of ndjson plain and as a `.seek.zst` of several frames, in a directory that is
/// kept for the test.
fn fixtures(dir: &Path) -> (PathBuf, PathBuf) {
    let text: String = (0..40)
        .map(|i| format!("{{\"n\":{i},\"name\":\"row{i}\"}}\n"))
        .collect();
    let plain = dir.join("events.jsonl");
    std::fs::write(&plain, &text).expect("write the plain file");
    let seek_zst = dir.join("events.jsonl.seek.zst");
    let mut compressed = Vec::new();
    convert_to_seekable_zst_reader(text.as_bytes(), &mut compressed, 256, true, b"\n", None)
        .expect("compress to seekable zstd");
    std::fs::write(&seek_zst, compressed).expect("write the seek.zst file");
    (plain, seek_zst)
}

#[test]
fn ssh_reads_the_same_rows_as_the_local_file() {
    let Some(target) = target() else {
        println!("skipped: POLARS_DYN_SSH_TARGET is not set");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let (plain, seek_zst) = fixtures(dir.path());
    let local = collect_nuon(&plain.display().to_string(), "");
    assert!(local.contains("[39, \"row39\"]"), "{local}");

    // ssh → ndjson
    let remote = format!("ssh://{target}{}", plain.display());
    assert_eq!(collect_nuon(&remote, ""), local);

    // ssh → seek-zst → ndjson, in chunks that straddle frames
    let remote = format!("ssh://{target}{}", seek_zst.display());
    assert_eq!(
        collect_nuon(&remote, "--opts {ndjson: {chunk_size: 256}}"),
        local
    );

    // a filter and a first, which push the predicate and n_rows into the chunked scan
    let filtered = run_nu(&format!(
        "polars_dyn open {remote} --opts {{ssh: {{port: 22}}, ndjson: {{chunk_size: 256}}}} \
         | polars_dyn filter ((polars_dyn col n) > 30) | polars_dyn first 2 \
         | polars_dyn collect | polars_dyn into-nu | to nuon"
    ));
    assert_eq!(
        filtered.trim(),
        "[[n, name]; [31, \"row31\"], [32, \"row32\"]]"
    );
}

#[test]
fn ssh_reports_a_missing_file_and_a_bad_option() {
    let Some(target) = target() else {
        println!("skipped: POLARS_DYN_SSH_TARGET is not set");
        return;
    };
    let fail = |script: &str| {
        let output = Command::new("nu")
            .args([
                "--no-config-file",
                "--plugins",
                &format!("[{}]", plugin().display()),
                "-c",
                script,
            ])
            .output()
            .expect("`nu` must be on PATH to run the integration tests");
        assert!(!output.status.success(), "nu succeeded unexpectedly");
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    let stderr = fail(&format!(
        "polars_dyn open ssh://{target}/nonexistent/x.jsonl"
    ));
    assert!(stderr.contains("ssh scan error"), "stderr:\n{stderr}");
    let stderr = fail(&format!(
        "polars_dyn open ssh://{target}/x.jsonl --opts {{ssh: {{prot: 22}}}}"
    ));
    assert!(stderr.contains("unknown field `prot`"), "stderr:\n{stderr}");
}
