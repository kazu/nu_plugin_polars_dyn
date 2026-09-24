//! `polars_dyn open` reads the built-in formats back as a LazyFrame, resolves the chain from the
//! scheme and the suffixes, cuts `--opts` by scan name, and reports what it cannot resolve with
//! the registered names. Runs the `nu` on `PATH` against the built plugin binary.

use std::process::{Command, Output};

fn nu(script: &str) -> Output {
    let plugin = env!("CARGO_BIN_EXE_nu_plugin_polars_dyn");
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

const TABLE: &str = "[[a b]; [1 2] [3 4]]";
/// `TABLE` as `to nuon` prints it.
const NUON: &str = "[[a, b]; [1, 2], [3, 4]]";

/// Writes `TABLE` with `polars_dyn save`, opens it again and returns the frame as nuon.
fn round_trip(file_name: &str) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(file_name).display().to_string();
    run_nu(&format!(
        "{TABLE} | polars_dyn into-df | polars_dyn save {path}; \
         polars_dyn open {path} | polars_dyn collect | polars_dyn into-nu | to nuon"
    ))
}

#[test]
fn opens_parquet() {
    assert_eq!(round_trip("x.parquet").trim(), NUON);
}

#[test]
fn opens_csv() {
    assert_eq!(round_trip("x.csv").trim(), NUON);
}

#[test]
fn opens_arrow() {
    assert_eq!(round_trip("x.arrow").trim(), NUON);
}

#[test]
fn opens_ndjson() {
    assert_eq!(round_trip("x.ndjson").trim(), NUON);
}

#[test]
fn open_returns_a_lazyframe() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("x.parquet").display().to_string();
    let out = run_nu(&format!(
        "{TABLE} | polars_dyn into-df | polars_dyn save {path}; \
         polars_dyn open {path} | describe"
    ));
    assert_eq!(out.trim(), "polars_lazyframe");
}

/// `--opts` is keyed by the scan's name, and the record under it reaches polars' options.
#[test]
fn csv_opts_reach_the_polars_options() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("x.csv").display().to_string();
    let out = run_nu(&format!(
        "{TABLE} | polars_dyn into-df | polars_dyn save {path}; \
         polars_dyn open {path} --opts {{csv: {{has_header: false}}}} | polars_dyn collect | polars_dyn into-nu | to nuon"
    ));
    assert_eq!(
        out.trim(),
        r#"[["column_1", "column_2"]; [a, b], ["1", "2"], ["3", "4"]]"#
    );
}

/// The `scan` key of parquet reaches `UnifiedScanArgs`: polars' own scan reads the bytes.
#[test]
fn parquet_scan_opts_reach_the_unified_scan_args() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("x.parquet").display().to_string();
    let out = run_nu(&format!(
        "{TABLE} | polars_dyn into-df | polars_dyn save {path}; \
         polars_dyn open {path} --opts {{parquet: {{scan: {{row_index: {{name: i, offset: 10}}}}}}}} \
         | polars_dyn collect | polars_dyn into-nu | to nuon"
    ));
    assert_eq!(out.trim(), "[[i, a, b]; [10, 1, 2], [11, 3, 4]]");
}

/// A csv or ndjson is cut into chunks of `chunk_size` bytes, each moved to the next newline, and
/// the chunks read in parallel give the rows the single chunk gives.
#[test]
fn csv_and_ndjson_read_the_same_rows_in_small_chunks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rows: String = (0..200).map(|i| format!("{i},row{i}\n")).collect();
    let csv = dir.path().join("x.csv");
    std::fs::write(&csv, format!("n,name\n{rows}")).expect("write fixture");
    let ndjson = dir.path().join("x.ndjson");
    let lines: String = (0..200)
        .map(|i| format!("{{\"n\":{i},\"name\":\"row{i}\"}}\n"))
        .collect();
    std::fs::write(&ndjson, lines).expect("write fixture");

    for (path, name) in [(&csv, "csv"), (&ndjson, "ndjson")] {
        let path = path.display();
        let whole = run_nu(&format!(
            "polars_dyn open {path} | polars_dyn collect | polars_dyn into-nu | to nuon"
        ));
        let chunked = run_nu(&format!(
            "polars_dyn open {path} --opts {{{name}: {{chunk_size: 100}}}} \
             | polars_dyn collect | polars_dyn into-nu | to nuon"
        ));
        assert_eq!(chunked, whole, "{name}");
        assert!(whole.contains("[199, \"row199\"]"), "{name}: {whole}");
    }
}

/// A final line longer than the distance to the next cut leaves no empty chunk after it: the
/// 200-row csv above ends in a row that no cut can split, and the same holds when the cut lands
/// exactly on the last newline.
#[test]
fn csv_final_line_past_the_last_cut_reads_the_same_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("x.csv");
    // the second row is 300 bytes; a chunk_size of 100 puts every cut target inside it
    let long = "y".repeat(290);
    std::fs::write(&path, format!("n,name\n1,a\n2,{long}\n")).expect("write fixture");
    let path = path.display();
    let out = run_nu(&format!(
        "polars_dyn open {path} --opts {{csv: {{chunk_size: 100}}}} \
         | polars_dyn collect | polars_dyn into-nu | get n | to nuon"
    ));
    assert_eq!(out.trim(), "[1, 2]");
}

/// The cut does not know csv quoting: a newline inside a quoted field is read whole when the
/// file is one chunk, and is a cut like any other when it is the first past a chunk boundary,
/// which leaves an unterminated quote for polars to refuse. The second is the documented limit,
/// held here so a change shows.
#[test]
fn csv_quoted_newline_is_whole_in_one_chunk_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("x.csv");
    std::fs::write(&path, "n,name\n1,\"a\nb\"\n2,c\n").expect("write fixture");
    let path = path.display();
    let whole = run_nu(&format!(
        "polars_dyn open {path} | polars_dyn collect | polars_dyn into-nu | to nuon"
    ));
    assert_eq!(whole.trim(), "[[n, name]; [1, \"a\nb\"], [2, c]]");
    let stderr = fail_nu(&format!(
        "polars_dyn open {path} --opts {{csv: {{chunk_size: 10}}}} | polars_dyn collect"
    ));
    assert!(stderr.contains("could not parse"), "stderr:\n{stderr}");
}

#[test]
fn format_flag_overrides_the_suffix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let csv = dir.path().join("x.csv").display().to_string();
    let txt = dir.path().join("x.txt").display().to_string();
    let out = run_nu(&format!(
        "{TABLE} | polars_dyn into-df | polars_dyn save {csv}; mv {csv} {txt}; \
         polars_dyn open {txt} --format csv | polars_dyn collect | polars_dyn into-nu | to nuon"
    ));
    assert_eq!(out.trim(), NUON);
}

/// `file://` is the scheme a path without one gets.
#[test]
fn file_scheme_opens_a_local_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("x.csv").display().to_string();
    let out = run_nu(&format!(
        "{TABLE} | polars_dyn into-df | polars_dyn save {path}; \
         polars_dyn open file://{path} | polars_dyn collect | polars_dyn into-nu | to nuon"
    ));
    assert_eq!(out.trim(), NUON);
}

#[test]
fn unknown_suffix_lists_the_registered_names() {
    let stderr = fail_nu("polars_dyn open data.xyz");
    assert!(
        stderr.contains("No scan source for") && stderr.contains("data.xyz"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("registered: parquet, csv, ipc, ndjson"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn unknown_format_lists_the_registered_names() {
    let stderr = fail_nu("polars_dyn open data.xyz --format avro");
    assert!(
        stderr.contains("No scan source named `avro`"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("registered: file, parquet, csv, ipc, ndjson"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn unknown_scheme_lists_the_registered_schemes() {
    let stderr = fail_nu("polars_dyn open ftp://h/x.jsonl");
    assert!(
        stderr.contains("No scan source for scheme `ftp`"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("registered schemes: file"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn unknown_opt_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("x.csv").display().to_string();
    let stderr = fail_nu(&format!(
        "{TABLE} | polars_dyn into-df | polars_dyn save {path}; \
         polars_dyn open {path} --opts {{csv: {{has_hedar: false}}}} | polars_dyn collect"
    ));
    assert!(
        stderr.contains("unknown option `has_hedar`"),
        "stderr:\n{stderr}"
    );
}

/// A key of `--opts` that names no scan of the chain is an error naming the chain.
#[test]
fn opts_for_a_scan_not_in_the_chain_is_an_error() {
    let stderr = fail_nu("polars_dyn open x.jsonl --opts {nope: {}}");
    assert!(
        stderr.contains("unknown scan `nope` in --opts"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("the chain is: file, ndjson"),
        "stderr:\n{stderr}"
    );
}

/// A csv option that picks rows or columns out of the file would be applied once per chunk, so it
/// is refused rather than answered differently than polars' own reader would.
#[test]
fn csv_refuses_options_the_chunks_cannot_honour() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("x.csv");
    std::fs::write(&path, "a,b\n1,2\n").expect("write fixture");
    let path = path.display();
    for (option, value, what) in [
        ("n_rows", "3", "picks rows"),
        ("skip_rows", "2", "picks rows"),
        ("skip_lines", "2", "picks rows"),
        ("skip_rows_after_header", "2", "picks rows"),
        ("row_index", "{name: i, offset: 0}", "numbers the rows"),
        ("column_names_overwrite", "[a, b]", "names the columns"),
        ("columns", "[name]", "picks columns"),
        ("projection", "[1]", "picks columns"),
    ] {
        let stderr = fail_nu(&format!(
            "polars_dyn open {path} --opts {{csv: {{{option}: {value}}}}} | polars_dyn collect"
        ));
        assert!(
            stderr.contains(&format!("`{option}` {what}")),
            "expected {option} to be refused, got {stderr}"
        );
    }
    let stderr = fail_nu(&format!(
        "polars_dyn open {path} --opts {{csv: {{scan: {{glob: false}}}}}} | polars_dyn collect"
    ));
    assert!(
        stderr.contains("unknown option `scan`"),
        "the scan key is not taken by csv, got {stderr}"
    );
}

/// The files a glob fixture holds, in the order of their paths as strings.
const GLOB_FILES: [&str; 3] = ["x1", "x10", "x2"];

/// Saves a one-row frame per name of `GLOB_FILES` as `<name>.<ext>`, each with its own row, and
/// returns the directory. A `jsonl` file is saved as ndjson and renamed.
fn glob_fixture(ext: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let save_ext = if ext == "jsonl" { "ndjson" } else { ext };
    for name in GLOB_FILES.iter().rev() {
        let saved = dir.path().join(format!("{name}.{save_ext}"));
        run_nu(&format!(
            "[[a b]; [{} {name}]] | polars_dyn into-df | polars_dyn save {}",
            name.len(),
            saved.display()
        ));
        std::fs::rename(&saved, dir.path().join(format!("{name}.{ext}"))).expect("rename");
    }
    dir
}

/// Opens each file of `GLOB_FILES` on its own and appends the frames, as nuon.
fn one_by_one_nuon(dir: &tempfile::TempDir, ext: &str) -> String {
    let paths: Vec<String> = GLOB_FILES
        .iter()
        .map(|name| format!("'{}'", dir.path().join(format!("{name}.{ext}")).display()))
        .collect();
    run_nu(&format!(
        "[{}] | each {{|p| polars_dyn open $p | polars_dyn collect | polars_dyn into-nu }} \
         | flatten | to nuon",
        paths.join(" ")
    ))
    .trim()
    .to_owned()
}

/// A glob of local files opens each match and stacks the frames in the order of the paths.
#[test]
fn glob_stacks_the_matches_in_path_order() {
    for ext in ["jsonl", "csv", "parquet", "arrow"] {
        let dir = glob_fixture(ext);
        let out = run_nu(&format!(
            "polars_dyn open '{}/x*.{ext}' | polars_dyn collect | polars_dyn into-nu | to nuon",
            dir.path().display()
        ));
        assert_eq!(out.trim(), one_by_one_nuon(&dir, ext), "{ext}");
        assert_eq!(
            out.trim(),
            r#"[[a, b]; [2, "x1"], [3, "x10"], [2, "x2"]]"#,
            "{ext}"
        );
    }
}

/// A relative glob is expanded against the engine's current directory.
#[test]
fn relative_glob_is_expanded_in_the_current_directory() {
    let dir = glob_fixture("csv");
    let out = run_nu(&format!(
        "cd '{}'; polars_dyn open 'x*.csv' | polars_dyn collect | polars_dyn into-nu | to nuon",
        dir.path().display()
    ));
    assert_eq!(out.trim(), one_by_one_nuon(&dir, "csv"));
}

/// As polars' own path scan does, a glob skips directories and empty files and orders the matches
/// as strings: `a-b/x.csv` comes before `a/x.csv`.
#[test]
fn glob_skips_directories_and_empty_files_and_sorts_as_strings() {
    let dir = tempfile::tempdir().expect("tempdir");
    for sub in ["a", "a-b", "a/sub.csv"] {
        std::fs::create_dir_all(dir.path().join(sub)).expect("create dir");
    }
    std::fs::write(dir.path().join("a/x.csv"), "n\n1\n").expect("write fixture");
    std::fs::write(dir.path().join("a-b/x.csv"), "n\n2\n").expect("write fixture");
    std::fs::write(dir.path().join("a/y.csv"), "").expect("write fixture");
    let out = run_nu(&format!(
        "polars_dyn open '{}/*/*.csv' | polars_dyn collect | polars_dyn into-nu | to nuon",
        dir.path().display()
    ));
    assert_eq!(out.trim(), "[[n]; [2], [1]]");
}

/// As in upstream, a glob metacharacter is looked for in the absolute path, so a current
/// directory `a[1]` makes `x.csv` a glob that matches `a1/x.csv`.
#[test]
fn glob_is_looked_for_in_the_current_directory_too() {
    let dir = tempfile::tempdir().expect("tempdir");
    for sub in ["a[1]", "a1"] {
        std::fs::create_dir(dir.path().join(sub)).expect("create dir");
    }
    std::fs::write(dir.path().join("a1/x.csv"), "n\n1\n").expect("write fixture");
    let out = run_nu(&format!(
        "cd '{}'; polars_dyn open x.csv | polars_dyn collect | polars_dyn into-nu | to nuon",
        dir.path().join("a[1]").display()
    ));
    assert_eq!(out.trim(), "[[n]; [1]]");
}

#[test]
fn glob_without_a_match_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pattern = format!("{}/y*.csv", dir.path().display());
    let stderr = fail_nu(&format!("polars_dyn open '{pattern}'"));
    assert!(
        stderr.contains(&format!("No file matches `{pattern}`")),
        "stderr:\n{stderr}"
    );
}

/// Files whose columns differ are not aligned: the error of polars' `concat` comes out.
#[test]
fn glob_of_files_with_other_columns_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("x1.csv"), "a,b\n1,2\n").expect("write fixture");
    std::fs::write(dir.path().join("x2.csv"), "a,c\n3,4\n").expect("write fixture");
    let stderr = fail_nu(&format!(
        "polars_dyn open '{}/x*.csv' | polars_dyn collect",
        dir.path().display()
    ));
    assert!(
        stderr.contains("unable to vstack, column names don't match"),
        "stderr:\n{stderr}"
    );
}
