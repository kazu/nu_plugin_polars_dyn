//! `nu_plugin_polars_dyn` plus the compile-in `logfmt` scan source. Registration and usage are
//! in `docs.dev/dev_bin.md`.

mod logfmt;

use nu_plugin::{MsgPackSerializer, serve_plugin};
use nu_plugin_polars::{
    PolarsPlugin,
    scan::{ScanSource, builtin::BUILTIN},
};

fn main() {
    env_logger::init();

    // SAFETY: the process is still single-threaded; `serve_plugin` below spawns the first thread,
    // so no other thread can read the environment while it is being written.
    unsafe {
        std::env::set_var("POLARS_ALLOW_EXTENSION", "true");
    }
    let sources: Vec<&'static dyn ScanSource> = BUILTIN
        .iter()
        .copied()
        .chain([&logfmt::Logfmt as &dyn ScanSource])
        .collect();
    match PolarsPlugin::new(Box::leak(sources.into_boxed_slice())) {
        Ok(ref plugin) => serve_plugin(plugin, MsgPackSerializer {}),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
