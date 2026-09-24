//! `polars_dyn open` on a `.seek.zst` source reads the same rows as the plain text file: the
//! chain `file`, `seek-zst`, `ndjson` (or `csv`) hands the decompressed bytes to the built-in
//! reader, which cuts them at newlines across frame boundaries and applies the pushdowns polars
//! hands an anonymous scan. The `seek-zst` source lives in the `seekzstdsep_scan` crate, so this
//! builds a plugin with that crate compiled in and runs the `nu` on `PATH` against it.
//!
//! The fixtures are a few kilobytes, which the default `chunk_size` reads as one chunk, so the
//! `.seek.zst` side is read with `chunk_size: 256` to make the chunks straddle frames and the
//! plain side with the default: what has to agree is a many-chunk read over decompressed frames
//! and a one-chunk read of the text.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use seekzstdsep::{RecordReader, convert_to_seekable_zst_reader};
use tempfile::TempDir;

/// The plugin every test here runs against, built once for the whole file.
fn plugin() -> &'static Path {
    static PLUGIN: OnceLock<PathBuf> = OnceLock::new();
    PLUGIN.get_or_init(|| {
        common::plugin_with(
            &common::plugin_dir("seek_zst"),
            &[("seekzstdsep_scan", Path::new("seekzstdsep-scan"))],
        )
    })
}

fn nu(script: &str) -> Output {
    nu_with_threads(script, None)
}

/// Runs `script`, holding polars to `threads` worker threads when one is given.
///
/// The scan reads a batch of chunks at a time when it is given an `n_rows`, and the batch is as
/// wide as the thread pool, so a file of fewer chunks than the machine has threads never reaches
/// the second batch. Asking for two threads is what makes the fixtures span batches.
fn nu_with_threads(script: &str, threads: Option<&str>) -> Output {
    let mut command = Command::new("nu");
    command.args([
        "--no-config-file",
        "--plugins",
        &format!("[{}]", plugin().display()),
        "-c",
        script,
    ]);
    if let Some(threads) = threads {
        command.env("POLARS_MAX_THREADS", threads);
    }
    command
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

/// Writes `text` both plain and as a `.seek.zst`, and returns the two paths.
///
/// Fails when `frame_size` leaves the text in a single frame: a one-frame file exercises none of
/// the frame crossing this module is about, and a test written against one passes without
/// covering it.
fn fixture(dir: &TempDir, name: &str, text: &str, frame_size: usize) -> (PathBuf, PathBuf) {
    let plain = dir.path().join(name);
    std::fs::write(&plain, text).expect("write the plain file");

    let seek_zst = dir.path().join(format!("{name}.seek.zst"));
    let mut compressed = Vec::new();
    convert_to_seekable_zst_reader(
        text.as_bytes(),
        &mut compressed,
        frame_size,
        true,
        b"\n",
        None,
    )
    .expect("compress to seekable zstd");
    std::fs::write(&seek_zst, compressed).expect("write the seek.zst file");

    let frames = RecordReader::open(seek_zst.clone(), b"\n")
        .expect("open the seek.zst fixture")
        .frame_count();
    assert!(
        frames > 1,
        "{name} was written as {frames} frame(s); lower frame_size or lengthen the text"
    );

    (plain, seek_zst)
}

/// The `--opts` that reads a `.seek.zst` fixture in many chunks; the plain file takes none.
fn opts_for(path: &Path) -> &'static str {
    let name = path.to_string_lossy();
    match (name.ends_with(".seek.zst"), name.contains(".csv")) {
        (false, _) => "",
        (true, true) => "--opts {csv: {chunk_size: 256}}",
        (true, false) => "--opts {ndjson: {chunk_size: 256}}",
    }
}

/// Runs `polars_dyn open <path> <opts> | <query> | collect` and returns the frame as nuon.
fn query_nuon(path: &Path, query: &str) -> String {
    let opts = opts_for(path);
    let path = path.display();
    let query = if query.is_empty() {
        String::new()
    } else {
        format!("{query} | ")
    };
    run_nu(&format!(
        "polars_dyn open {path} {opts} | {query}polars_dyn collect \
         | polars_dyn into-nu | to nuon"
    ))
    .trim()
    .to_owned()
}

fn collect_nuon(path: &Path) -> String {
    query_nuon(path, "")
}

/// 40 rows of ndjson, enough to span several frames at the frame size the fixtures use.
fn ndjson_text() -> String {
    (0..40)
        .map(|i| format!("{{\"n\":{i},\"name\":\"row{i}\"}}\n"))
        .collect()
}

/// 100 rows of csv, which the frame size the fixtures use splits across several frames.
fn csv_text() -> String {
    let rows: String = (0..100).map(|i| format!("{i},row{i}\n")).collect();
    format!("n,name\n{rows}")
}

#[test]
fn ndjson_seek_zst_matches_the_plain_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plain, seek_zst) = fixture(&dir, "events.jsonl", &ndjson_text(), 256);
    assert_eq!(collect_nuon(&seek_zst), collect_nuon(&plain));
    assert!(collect_nuon(&plain).contains("[39, \"row39\"]"));
}

#[test]
fn csv_seek_zst_matches_the_plain_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plain, seek_zst) = fixture(&dir, "data.csv", &csv_text(), 256);
    assert_eq!(collect_nuon(&seek_zst), collect_nuon(&plain));
}

/// The header is in chunk 0 only, so a later chunk must not lose its first row to it.
#[test]
fn csv_seek_zst_reads_the_header_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_, seek_zst) = fixture(&dir, "data.csv", &csv_text(), 256);
    let opts = opts_for(&seek_zst);
    let path = seek_zst.display();
    let rows = run_nu(&format!(
        "polars_dyn open {path} {opts} | polars_dyn collect | polars_dyn into-nu | length"
    ));
    assert_eq!(rows.trim(), "100", "every row but the header must survive");
}

/// A glob of `.seek.zst` files runs the chain once per match and stacks the frames in the order
/// of the paths, as opening each file on its own and appending does.
#[test]
fn ndjson_seek_zst_glob_stacks_the_matches_in_path_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let later: String = (40..80)
        .map(|i| format!("{{\"n\":{i},\"name\":\"row{i}\"}}\n"))
        .collect();
    let (_, second) = fixture(&dir, "x2.jsonl", &later, 256);
    let (_, first) = fixture(&dir, "x1.jsonl", &ndjson_text(), 256);
    let opts = opts_for(&first);
    let one_by_one = run_nu(&format!(
        "['{}' '{}'] | each {{|p| polars_dyn open $p {opts} | polars_dyn collect \
         | polars_dyn into-nu }} | flatten | to nuon",
        first.display(),
        second.display()
    ));
    let globbed = run_nu(&format!(
        "polars_dyn open '{}/*.jsonl.seek.zst' {opts} | polars_dyn collect \
         | polars_dyn into-nu | to nuon",
        dir.path().display()
    ));
    assert_eq!(globbed, one_by_one);
    assert!(globbed.contains("[0, \"row0\"]") && globbed.contains("[79, \"row79\"]"));
}

/// `n_rows` is the one slice polars pushes into an anonymous scan.
#[test]
fn ndjson_seek_zst_takes_the_first_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plain, seek_zst) = fixture(&dir, "events.jsonl", &ndjson_text(), 256);
    for path in [&seek_zst, &plain] {
        assert_eq!(
            query_nuon(path, "polars_dyn slice 0 3"),
            "[[n, name]; [0, \"row0\"], [1, \"row1\"], [2, \"row2\"]]",
            "for {}",
            path.display()
        );
    }
}

/// The predicate is pushed down and applied per chunk, so it has to select the same rows the
/// plain scan does, including rows that are not in the first chunk.
#[test]
fn ndjson_seek_zst_applies_the_pushed_down_predicate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plain, seek_zst) = fixture(&dir, "events.jsonl", &ndjson_text(), 256);
    let query = "polars_dyn filter ((polars_dyn col n) > 36)";
    assert_eq!(
        query_nuon(&seek_zst, query),
        "[[n, name]; [37, \"row37\"], [38, \"row38\"], [39, \"row39\"]]"
    );
    assert_eq!(query_nuon(&seek_zst, query), query_nuon(&plain, query));
}

/// The projection is pushed down and dropped per chunk, so only the asked-for column comes back.
#[test]
fn ndjson_seek_zst_applies_the_pushed_down_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plain, seek_zst) = fixture(&dir, "events.jsonl", &ndjson_text(), 256);
    let query = "polars_dyn select (polars_dyn col name) | polars_dyn slice 0 2";
    assert_eq!(
        query_nuon(&seek_zst, query),
        "[[name]; [\"row0\"], [\"row1\"]]"
    );
    assert_eq!(query_nuon(&seek_zst, query), query_nuon(&plain, query));
}

/// The format's options go to the format under its name, and `seek-zst` takes none.
#[test]
fn seek_zst_opts_are_cut_by_scan_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_, seek_zst) = fixture(&dir, "data.csv", &csv_text(), 256);
    let path = seek_zst.display();

    let nuon = run_nu(&format!(
        "polars_dyn open {path} --opts {{csv: {{has_header: false}}}} | polars_dyn slice 0 1 \
         | polars_dyn collect | polars_dyn into-nu | to nuon"
    ));
    assert_eq!(nuon.trim(), "[[\"column_1\", \"column_2\"]; [n, name]]");

    let stderr = fail_nu(&format!(
        "polars_dyn open {path} --opts {{seek-zst: {{verify_frames: false}}}} | polars_dyn collect"
    ));
    assert!(
        stderr.contains("`seek-zst` takes no options, got `verify_frames`"),
        "got {stderr}"
    );

    let stderr = fail_nu(&format!(
        "polars_dyn open {path} --opts {{nope: {{}}}} | polars_dyn collect"
    ));
    assert!(
        stderr.contains("the chain is: file, seek-zst, csv"),
        "got {stderr}"
    );
}

/// `--format` names the chain when the suffixes do not, and a chain that ends in `seek-zst` has
/// nothing to read the bytes into a frame.
#[test]
fn seek_zst_chain_is_named_by_format_or_ends_early() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plain, seek_zst) = fixture(&dir, "events.jsonl", &ndjson_text(), 256);
    let bin = dir.path().join("dump.bin");
    std::fs::copy(&seek_zst, &bin).expect("copy the fixture");
    let path = bin.display();
    let named = run_nu(&format!(
        "polars_dyn open {path} --format seek-zst,ndjson --opts {{ndjson: {{chunk_size: 256}}}} \
         | polars_dyn collect | polars_dyn into-nu | to nuon"
    ));
    assert_eq!(named.trim(), collect_nuon(&plain));

    let stderr = fail_nu(&format!("polars_dyn open {path} --format seek-zst"));
    assert!(
        stderr.contains("`seek-zst` does not read bytes into a frame"),
        "got {stderr}"
    );
    let bare = dir.path().join("x.seek.zst");
    std::fs::copy(&seek_zst, &bare).expect("copy the fixture");
    let stderr = fail_nu(&format!("polars_dyn open {}", bare.display()));
    assert!(
        stderr.contains("`seek-zst` does not read bytes into a frame"),
        "got {stderr}"
    );
}

/// polars-stream has no anonymous scan, so the streaming engine cannot collect this source.
#[test]
fn seek_zst_cannot_collect_on_the_streaming_engine() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_, seek_zst) = fixture(&dir, "events.jsonl", &ndjson_text(), 256);
    let path = seek_zst.display();
    let stderr = fail_nu(&format!(
        "polars_dyn open {path} | polars_dyn collect --streaming"
    ));
    assert!(
        stderr.contains("AnonymousScan"),
        "expected the anonymous scan error, got {stderr}"
    );
}

/// Records of widely varying length put the frame boundaries at uneven byte positions, and the
/// chunk cuts fall wherever the next newline is. The rows still come back whole and in order.
#[test]
fn ndjson_seek_zst_reads_records_of_varying_length() {
    let dir = tempfile::tempdir().expect("tempdir");
    let text: String = (0..40)
        .map(|i| {
            let msg = "x".repeat(if i % 2 == 0 { 1 } else { 400 });
            format!("{{\"n\":{i},\"msg\":\"{msg}\"}}\n")
        })
        .collect();
    let (plain, seek_zst) = fixture(&dir, "uneven.jsonl", &text, 4096);
    assert_eq!(collect_nuon(&seek_zst), collect_nuon(&plain));
}

/// A file whose frames hold different record counts is read by decompressed offset, so it gives
/// the plain file's rows. (Up to 016 it was refused, since frames were looked up by record number.)
#[test]
fn seek_zst_reads_frames_of_uneven_record_counts() {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, text) in [
        ("uneven.csv", uneven_csv_text()),
        ("uneven.jsonl", uneven_ndjson_text()),
    ] {
        let (plain, seek_zst) = uneven_fixture(&dir, name, &text);
        assert_eq!(collect_nuon(&seek_zst), collect_nuon(&plain), "{name}");
    }
}

/// `sort-by` before a `slice` pushes down a dynamic bound that only the optimizer can read. It is
/// left unapplied rather than lowered back, which would abort the process.
#[test]
fn ndjson_seek_zst_sorts_before_a_slice() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plain, seek_zst) = fixture(&dir, "events.jsonl", &ndjson_text(), 256);
    let query = "polars_dyn sort-by [n] | polars_dyn slice 0 2";
    assert_eq!(
        query_nuon(&seek_zst, query),
        "[[n, name]; [0, \"row0\"], [1, \"row1\"]]"
    );
    assert_eq!(query_nuon(&seek_zst, query), query_nuon(&plain, query));
}

/// A filter and a `sort-by | slice` are pushed down as one `and`. The bound from the sort cannot be
/// evaluated here, but the filter must be, so the two are taken apart rather than judged together.
#[test]
fn ndjson_seek_zst_keeps_the_filter_beside_a_sort_bound() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plain, seek_zst) = fixture(&dir, "events.jsonl", &ndjson_text(), 256);
    let query = "polars_dyn filter ((polars_dyn col n) > 30) | polars_dyn sort-by [n] \
                 | polars_dyn slice 0 3 | polars_dyn select (polars_dyn col n)";
    assert_eq!(query_nuon(&seek_zst, query), "[[n]; [31], [32], [33]]");
    assert_eq!(query_nuon(&seek_zst, query), query_nuon(&plain, query));
}

/// `n_rows` counts rows of the file, so a `slice` before a `filter` asks for the matches among
/// those rows, not for that many matches.
#[test]
fn ndjson_seek_zst_slices_the_file_before_filtering() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plain, seek_zst) = fixture(&dir, "events.jsonl", &ndjson_text(), 256);
    let query = "polars_dyn slice 0 26 | polars_dyn filter ((polars_dyn col n) > 20) \
                 | polars_dyn select (polars_dyn col n)";
    assert_eq!(
        query_nuon(&seek_zst, query),
        "[[n]; [21], [22], [23], [24], [25]]"
    );
    assert_eq!(query_nuon(&seek_zst, query), query_nuon(&plain, query));
}

/// The queries whose answer a `.seek.zst` source has to give as the plain file does.
///
/// Each is what follows `polars_dyn open <file>`. They are the combinations of the three things a
/// query moves — `n_rows`, the projection and the predicate — since what this scan has had wrong
/// has been how those meet rather than any one of them: which comes first, what is left to the
/// plan, and where the chunk boundaries fall. The fourth thing polars pushes down, the schema,
/// does not change with the query; `ndjson_seek_zst_takes_a_schema_for_a_column_the_first_chunk_lacks`
/// is where it is answered for.
const QUERIES: &[&str] = &[
    // n_rows alone, around the chunk boundaries of both fixtures: 256 bytes is about 12 ndjson
    // rows and 31 csv rows, and the csv header takes a line of the first chunk.
    "polars_dyn slice 0 0",
    "polars_dyn slice 0 1",
    "polars_dyn slice 0 11",
    "polars_dyn slice 0 12",
    "polars_dyn slice 0 13",
    "polars_dyn slice 0 24",
    "polars_dyn slice 0 29",
    "polars_dyn slice 0 30",
    "polars_dyn slice 0 31",
    "polars_dyn slice 0 39",
    "polars_dyn slice 0 40",
    "polars_dyn slice 0 61",
    "polars_dyn slice 0 62",
    "polars_dyn slice 0 92",
    "polars_dyn slice 0 100",
    "polars_dyn slice 0 1000",
    // an offset, which polars does not push down
    "polars_dyn slice 5 5",
    "polars_dyn slice 30 5",
    // the predicate alone
    "polars_dyn filter ((polars_dyn col n) > 20)",
    "polars_dyn filter ((polars_dyn col n) > 1000)",
    "polars_dyn filter (((polars_dyn col n) > 10) and ((polars_dyn col n) < 20))",
    "polars_dyn filter (((polars_dyn col n) < 5) or ((polars_dyn col n) > 35))",
    // n_rows and the predicate, both orders
    "polars_dyn slice 0 26 | polars_dyn filter ((polars_dyn col n) > 20)",
    "polars_dyn filter ((polars_dyn col n) > 20) | polars_dyn slice 0 5",
    // the dynamic bound a sort before a slice produces
    "polars_dyn sort-by [n] | polars_dyn slice 0 3",
    "polars_dyn sort-by [n] --reverse [true] | polars_dyn slice 0 3",
    "polars_dyn filter ((polars_dyn col n) > 30) | polars_dyn sort-by [n] | polars_dyn slice 0 3",
    "polars_dyn sort-by [n] | polars_dyn slice 0 3 | polars_dyn filter ((polars_dyn col n) > 1)",
    "polars_dyn sort-by [n name] | polars_dyn slice 0 3",
    // the projection, including a filter on a column the projection drops
    "polars_dyn select (polars_dyn col name)",
    "polars_dyn select (polars_dyn col name) | polars_dyn slice 0 2",
    "polars_dyn filter ((polars_dyn col n) > 20) | polars_dyn select (polars_dyn col name)",
    "polars_dyn select (polars_dyn col name) | polars_dyn sort-by [name] | polars_dyn slice 0 2",
];

/// Runs every query in [`QUERIES`] against both files and fails on the first answer that differs.
fn assert_queries_match(plain: &Path, seek_zst: &Path) {
    // Two threads make the batch two chunks wide, narrower than either fixture's chunks, so
    // the queries reach the second batch and the `n_rows` that stops the scan early is answered
    // for. The default pool is wider than either has chunks, which would leave that path unread.
    for threads in [None, Some("2")] {
        for query in QUERIES {
            let answer = |path: &Path| {
                let opts = opts_for(path);
                let path = path.display();
                let script = format!(
                    "polars_dyn open {path} {opts} | {query} | polars_dyn collect \
                     | polars_dyn into-nu | to nuon"
                );
                let output = nu_with_threads(&script, threads);
                let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                assert!(
                    output.status.success(),
                    "nu failed for {query}\nstdout:\n{stdout}\nstderr:\n{stderr}"
                );
                stdout.trim().to_owned()
            };
            assert_eq!(
                answer(seek_zst),
                answer(plain),
                "differs from the plain file with threads={threads:?}: {query}"
            );
        }
    }
}

#[test]
fn ndjson_seek_zst_answers_every_query_as_the_plain_file_does() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plain, seek_zst) = fixture(&dir, "events.jsonl", &ndjson_text(), 256);
    assert_queries_match(&plain, &seek_zst);
}

#[test]
fn csv_seek_zst_answers_every_query_as_the_plain_file_does() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plain, seek_zst) = fixture(&dir, "data.csv", &csv_text(), 256);
    assert_queries_match(&plain, &seek_zst);
}

/// A column that only appears after the first chunk is missing, since the schema is settled there,
/// and passing the schema in `--opts` is what brings it back.
///
/// This is the one way out the module doc offers for the schema being read from one chunk, so it
/// is held to working.
#[test]
fn ndjson_seek_zst_takes_a_schema_for_a_column_the_first_chunk_lacks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let text: String = (0..80)
        .map(|i| {
            if i < 60 {
                format!("{{\"n\":{i}}}\n")
            } else {
                format!("{{\"n\":{i},\"extra\":\"x\"}}\n")
            }
        })
        .collect();
    let (plain, seek_zst) = fixture(&dir, "late.jsonl", &text, 256);
    let columns = |path: &Path, opts: &str| {
        let path = path.display();
        run_nu(&format!(
            "polars_dyn open {path} --opts {{ndjson: {{chunk_size: 256, {opts}}}}} \
             | polars_dyn collect | polars_dyn into-nu | columns | to nuon"
        ))
        .trim()
        .to_owned()
    };

    assert_eq!(
        columns(&plain, ""),
        "[n]",
        "the schema comes from the first chunk alone"
    );
    assert_eq!(columns(&seek_zst, ""), "[n]");

    let schema = "schema: {fields: {n: Int64, extra: String}, metadata: null}";
    assert_eq!(columns(&seek_zst, schema), "[n, extra]");
    assert_eq!(columns(&seek_zst, schema), columns(&plain, schema));
}

/// `schema_overwrite` is laid over whatever schema the scan settled on, and naming a column the
/// file does not have is the error it is for polars' own reader.
#[test]
fn ndjson_seek_zst_overwrites_the_schema_as_the_plain_file_does() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plain, seek_zst) = fixture(&dir, "events.jsonl", &ndjson_text(), 256);
    let dtypes = |path: &Path, opts: &str| {
        let path = path.display();
        run_nu(&format!(
            "polars_dyn open {path} --opts {{ndjson: {{{opts}}}}} | polars_dyn collect \
             | polars_dyn schema | to nuon"
        ))
        .trim()
        .to_owned()
    };

    // over an inferred schema it takes, and `n` comes back as a string rather than the i64 it
    // would be inferred as
    let over_inferred = "schema_overwrite: {fields: {n: String}, metadata: null}";
    assert_eq!(dtypes(&seek_zst, over_inferred), "{n: str, name: str}");
    assert_eq!(
        dtypes(&seek_zst, over_inferred),
        dtypes(&plain, over_inferred)
    );

    // beside a `schema` it does not, which is what polars' own reader answers with
    let over_given = "schema: {fields: {n: Int64, name: String}, metadata: null}, \
                      schema_overwrite: {fields: {n: String}, metadata: null}";
    assert_eq!(dtypes(&seek_zst, over_given), dtypes(&plain, over_given));
    assert_ne!(
        dtypes(&seek_zst, over_given),
        dtypes(&seek_zst, over_inferred)
    );

    // a column the file does not have is an error, not a line quietly ignored
    let missing = "schema_overwrite: {fields: {nope: String}, metadata: null}";
    let path = seek_zst.display();
    let stderr = fail_nu(&format!(
        "polars_dyn open {path} --opts {{ndjson: {{{missing}}}}} | polars_dyn collect"
    ));
    assert!(
        stderr.contains("nope"),
        "expected the missing column to be named, got {stderr}"
    );
}

/// Writes `text` as a `.seek.zst` whose frames are cut by bytes alone, so each holds whatever
/// record count `frame_size` leaves it, and returns the plain file and the `.seek.zst`.
///
/// `text` is short records followed by long ones, so frame 0 holds more records than the frames
/// after it. Fails unless a read of the whole file through seekzstdsep's verifying reader is
/// refused, which is what makes it a file whose frames a record-number lookup could not use. The
/// limit multiplier is raised from the default 4: the compressor refuses a text longer than
/// `frame_size` times it, and this one is longer than 256 * 4 bytes.
fn uneven_fixture(dir: &TempDir, name: &str, text: &str) -> (PathBuf, PathBuf) {
    let plain = dir.path().join(name);
    std::fs::write(&plain, text).expect("write the plain file");

    let seek_zst = dir.path().join(format!("{name}.seek.zst"));
    let mut compressed = Vec::new();
    convert_to_seekable_zst_reader(
        text.as_bytes(),
        &mut compressed,
        256,
        false,
        b"\n",
        Some(128),
    )
    .expect("compress to seekable zstd");
    std::fs::write(&seek_zst, compressed).expect("write the seek.zst file");

    let refused = RecordReader::open(seek_zst.clone(), b"\n")
        .expect("open the seek.zst fixture")
        .verifying()
        .into_records()
        .any(|record| record.is_err());
    assert!(refused, "{name} holds the same record count in every frame");

    (plain, seek_zst)
}

/// 60 rows whose `name` is one byte for the first 30 and 40 bytes after.
fn growing_rows() -> impl Iterator<Item = (usize, String)> {
    (0..60).map(|i| (i, "x".repeat(if i < 30 { 1 } else { 40 })))
}

fn uneven_csv_text() -> String {
    let rows: String = growing_rows()
        .map(|(i, name)| format!("{i},{name}\n"))
        .collect();
    format!("n,name\n{rows}")
}

fn uneven_ndjson_text() -> String {
    growing_rows()
        .map(|(i, name)| format!("{{\"n\":{i},\"name\":\"{name}\"}}\n"))
        .collect()
}
