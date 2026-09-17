//! Builds a `nu_plugin_polars_dyn` with the given scan source crates compiled in.
//!
//! ```text
//! nu-polars-dyn-build <crate>... [--path <name>=<dir>]... [--git <name>=<url>]...
//!                     [--out <dir>] [--debug]
//! ```
//!
//! Each crate must expose `pub fn scan_sources() -> &'static [&'static ScanSource]`. The
//! generated project, and why it pins `nu_plugin_polars_dyn` the way it does, are in
//! `docs.dev/custom_build.md`.

use std::{
    collections::BTreeMap,
    env,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

const USAGE: &str = "usage: nu-polars-dyn-build <crate>... \
                     [--path <name>=<dir>]... [--git <name>=<url>]... [--out <dir>] [--debug]";

/// The plugin crate the generated project builds against, overriding the repository this
/// builder was compiled from. A directory, taken as a path dependency.
const SOURCE_OVERRIDE: &str = "NU_POLARS_DYN_SOURCE";

/// What the generated project calls its binary. It is renamed to `nu_plugin_polars_dyn` on the
/// way to `--out`, because a shared `CARGO_TARGET_DIR` would otherwise overwrite the plugin the
/// same directory already holds.
const GENERATED_BIN: &str = "nu-polars-dyn-custom";

/// The lock of the plugin this builder was compiled from. Cargo keeps the versions it already
/// finds, so the custom binary is built against the same dependency versions as the published
/// one instead of whatever is newest today.
const PLUGIN_LOCK: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.lock"));

/// Where the generated manifest takes one dependency from.
enum Source {
    CratesIo,
    Path(String),
    Git(String),
}

struct Args {
    /// The crates to compile in, in the order they were given.
    crates: Vec<String>,
    /// The `--path` and `--git` overrides, by crate name.
    overrides: BTreeMap<String, Source>,
    out: PathBuf,
    debug: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("nu-polars-dyn-build: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let args = parse_args(env::args().skip(1))?;
    let project = env::temp_dir().join(format!("nu-polars-dyn-build-{}", std::process::id()));
    write_project(&project, &args)?;

    let profile = if args.debug { "debug" } else { "release" };
    let target_dir = match env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => project.join("target"),
    };
    let status = build(&project, &target_dir, args.debug)?;
    if !status.success() {
        eprintln!(
            "nu-polars-dyn-build: the generated project is in {}",
            project.display()
        );
        return Ok(exit_code(status.code()));
    }

    let built = target_dir
        .join(profile)
        .join(format!("{GENERATED_BIN}{}", env::consts::EXE_SUFFIX));
    let placed = args
        .out
        .join(format!("nu_plugin_polars_dyn{}", env::consts::EXE_SUFFIX));
    fs::copy(&built, &placed)
        .map_err(|e| format!("copying {} to {}: {e}", built.display(), placed.display()))?;
    let _ = fs::remove_dir_all(&project);

    println!("{}", placed.display());
    Ok(ExitCode::SUCCESS)
}

fn parse_args(argv: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut crates = Vec::new();
    let mut overrides = BTreeMap::new();
    let mut out = None;
    let mut debug = false;

    let mut argv = argv;
    while let Some(arg) = argv.next() {
        let mut value = |flag: &str| {
            argv.next()
                .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--debug" => debug = true,
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--path" => {
                let (name, dir) = split_override("--path", &value("--path")?)?;
                overrides.insert(name, Source::Path(absolute(&dir)?));
            }
            "--git" => {
                let (name, url) = split_override("--git", &value("--git")?)?;
                overrides.insert(name, Source::Git(url));
            }
            _ if arg.starts_with('-') => return Err(format!("unknown flag {arg}\n{USAGE}")),
            _ => crates.push(check_crate_name(&arg)?),
        }
    }

    if crates.is_empty() {
        return Err(format!("no crate given\n{USAGE}"));
    }
    if let Some(name) = overrides.keys().find(|name| !crates.contains(name)) {
        return Err(format!(
            "--path/--git names `{name}`, which is not one of the crates to compile in"
        ));
    }

    let out = match out {
        Some(out) => out,
        None => env::current_dir().map_err(|e| format!("current directory: {e}"))?,
    };
    if !out.is_dir() {
        return Err(format!("--out {} is not a directory", out.display()));
    }

    Ok(Args {
        crates,
        overrides,
        out,
        debug,
    })
}

/// Splits `<name>=<value>`, checking that the name can stand in the generated source.
fn split_override(flag: &str, arg: &str) -> Result<(String, String), String> {
    let (name, value) = arg
        .split_once('=')
        .ok_or_else(|| format!("{flag} takes <name>=<value>, got `{arg}`\n{USAGE}"))?;
    if value.is_empty() {
        return Err(format!("{flag} {name}= has no value\n{USAGE}"));
    }
    Ok((check_crate_name(name)?, value.to_string()))
}

/// Resolves a directory the caller named, relative to their working directory, to an absolute
/// one.
///
/// Cargo resolves a `path` dependency against the manifest that names it, and the generated
/// manifest sits in a temporary directory, so a relative path would point somewhere else there.
fn absolute(dir: &str) -> Result<String, String> {
    fs::canonicalize(dir)
        .map(|dir| dir.display().to_string())
        .map_err(|e| format!("{dir}: {e}"))
}

/// The name goes into a manifest key and a Rust path, so nothing but a crate name may pass.
fn check_crate_name(name: &str) -> Result<String, String> {
    if name.is_empty() {
        return Err(format!("empty crate name\n{USAGE}"));
    }
    if let Some(c) = name
        .chars()
        .find(|c| !c.is_ascii_alphanumeric() && *c != '_' && *c != '-')
    {
        return Err(format!("`{name}` is not a crate name (`{c}`)\n{USAGE}"));
    }
    Ok(name.to_string())
}

fn write_project(project: &Path, args: &Args) -> Result<(), String> {
    let src = project.join("src");
    fs::create_dir_all(&src).map_err(|e| format!("creating {}: {e}", src.display()))?;
    write(&project.join("Cargo.toml"), &manifest(args)?)?;
    write(&project.join("Cargo.lock"), PLUGIN_LOCK)?;
    write(&src.join("main.rs"), &main_rs(&args.crates))
}

fn manifest(args: &Args) -> Result<String, String> {
    let compiled_in: String = args
        .crates
        .iter()
        .map(|name| {
            let source = args.overrides.get(name).unwrap_or(&Source::CratesIo);
            format!("{name} = {}\n", dependency(source))
        })
        .collect();
    Ok(format!(
        "[package]\n\
         name = \"{GENERATED_BIN}\"\n\
         version = \"0.0.0\"\n\
         edition = \"2024\"\n\
         publish = false\n\
         \n\
         [[bin]]\n\
         name = \"{GENERATED_BIN}\"\n\
         path = \"src/main.rs\"\n\
         \n\
         [dependencies]\n\
         nu_plugin_polars_dyn = \"={version}\"\n\
         {compiled_in}\n\
         [patch.crates-io]\n\
         nu_plugin_polars_dyn = {source}\n",
        version = env!("CARGO_PKG_VERSION"),
        source = plugin_dependency()?
    ))
}

/// Where the generated manifest patches `nu_plugin_polars_dyn` to: the repository and version
/// this builder was compiled from, so the two cannot drift apart.
///
/// It is a `[patch.crates-io]` entry rather than the dependency itself so that a compiled-in
/// crate asking for `nu_plugin_polars_dyn` from crates.io — which is how one is written once the
/// plugin is published — resolves to the same copy. Two copies would be two `ScanSource` traits.
fn plugin_dependency() -> Result<String, String> {
    if let Some(dir) = env::var_os(SOURCE_OVERRIDE) {
        let dir = absolute(&dir.to_string_lossy())?;
        return Ok(dependency(&Source::Path(dir)));
    }
    Ok(format!(
        "{{ git = {}, tag = {} }}",
        toml_string(env!("CARGO_PKG_REPOSITORY")),
        toml_string(env!("CARGO_PKG_VERSION"))
    ))
}

fn dependency(source: &Source) -> String {
    match source {
        Source::CratesIo => "\"*\"".to_string(),
        Source::Path(dir) => format!("{{ path = {} }}", toml_string(dir)),
        Source::Git(url) => format!("{{ git = {} }}", toml_string(url)),
    }
}

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn main_rs(crates: &[String]) -> String {
    let sources = crates
        .iter()
        .map(|name| format!("{}::scan_sources()", name.replace('-', "_")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("fn main() {{\n    nu_plugin_polars::serve(&[{sources}]);\n}}\n")
}

fn write(path: &Path, contents: &str) -> Result<(), String> {
    let mut file = File::create(path).map_err(|e| format!("creating {}: {e}", path.display()))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| format!("writing {}: {e}", path.display()))
}

fn build(
    project: &Path,
    target_dir: &Path,
    debug: bool,
) -> Result<std::process::ExitStatus, String> {
    let mut cargo = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cargo
        .arg("build")
        .arg("--manifest-path")
        .arg(project.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(target_dir);
    if !debug {
        cargo.arg("--release");
    }
    cargo.status().map_err(|e| format!("running cargo: {e}"))
}

fn exit_code(code: Option<i32>) -> ExitCode {
    match code.and_then(|code| u8::try_from(code).ok()) {
        Some(code) => ExitCode::from(code),
        None => ExitCode::FAILURE,
    }
}
