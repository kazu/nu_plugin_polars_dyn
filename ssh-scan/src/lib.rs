//! The `ssh://` scan source for `nu_plugin_polars_dyn`: a file on a remote host, opened over sftp,
//! as the bytes the rest of the chain reads.
//!
//! Build a plugin with it compiled in:
//!
//! ```nu
//! nu-polars-dyn-build ssh_scan --path ssh_scan=./ssh-scan
//! ```
//!
//! It opens: `ssh://[user@]host/path` puts `ssh` first in the chain, and the suffixes of `path`
//! pick what follows, so `ssh://host/var/log/events.jsonl.seek.zst` is `ssh`, `seek-zst`, `ndjson`
//! and nothing after `ssh` knows the bytes are remote. `user` defaults to `$USER`; the path is
//! absolute as written (`ssh://host/var/log/x` opens `/var/log/x`).
//!
//! ```nu
//! polars_dyn open ssh://host/var/log/events.jsonl | polars_dyn collect
//! polars_dyn open ssh://kazu@host/var/log/events.jsonl.seek.zst --opts {ssh: {port: 2222}} | polars_dyn collect
//! polars_dyn open ssh://host/var/log/app.logfmt --opts {ssh: {identity: ($env.HOME | path join .ssh id_ed25519)}} | polars_dyn collect
//! ```
//!
//! `--opts` under `ssh` is [`SshOpts`]: `port` (default 22) and `identity`, the private key file to
//! authenticate with; without it the ssh agent is asked.
//!
//! An sftp handle serves one request at a time, so the reads of a chain that parses in parallel
//! are serialized here while the decompressing and parsing stay parallel. Every read is a seek
//! and a read on the one handle; the connection lives as long as the frame does.

use std::io::{self, Read, Seek, SeekFrom};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use nu_plugin_polars::scan::{ReadAt, ScanSource, parse_opts};
use polars::prelude::{PolarsError, PolarsResult, polars_bail};
use serde::Deserialize;
use ssh2::{File, Session, Sftp};

/// The entry point `nu-polars-dyn-build` calls from the `main.rs` it generates.
pub fn scan_sources() -> &'static [&'static dyn ScanSource] {
    &[&Ssh]
}

/// What `--opts` takes under `ssh`.
#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SshOpts {
    /// The port to connect to. Default 22.
    pub port: Option<u16>,
    /// The private key file to authenticate with. Without it the ssh agent is asked.
    pub identity: Option<PathBuf>,
}

/// The `ssh` source: scheme `ssh`, open only.
pub struct Ssh;

impl ScanSource for Ssh {
    fn name(&self) -> &'static str {
        "ssh"
    }

    fn suffixes(&self) -> &'static [&'static str] {
        &[]
    }

    fn schemes(&self) -> &'static [&'static str] {
        &["ssh"]
    }

    fn open(&self, url: &str, opts: &[u8]) -> PolarsResult<Arc<dyn ReadAt>> {
        let opts: SshOpts = serde_json::from_value(serde_json::Value::Object(parse_opts(opts)?))
            .map_err(|e| PolarsError::ComputeError(format!("opts: {e}").into()))?;
        let target = Target::parse(url)?;
        RemoteFile::open(&target, &opts)
            .map(|file| Arc::new(file) as Arc<dyn ReadAt>)
            .map_err(|e| PolarsError::ComputeError(format!("{url}: {e}").into()))
    }
}

/// `ssh://[user@]host/path` taken apart.
struct Target {
    user: String,
    host: String,
    path: String,
}

impl Target {
    fn parse(url: &str) -> PolarsResult<Self> {
        let Some(rest) = url.strip_prefix("ssh://") else {
            polars_bail!(ComputeError: "`{url}` is not an ssh:// URL")
        };
        let Some((authority, path)) = rest.split_once('/') else {
            polars_bail!(ComputeError: "`{url}` has no path: expected ssh://[user@]host/path")
        };
        let (user, host) = match authority.split_once('@') {
            Some((user, host)) => (user.to_owned(), host),
            None => {
                let user = std::env::var("USER").map_err(|_| {
                    PolarsError::ComputeError(
                        format!("`{url}` names no user and $USER is not set").into(),
                    )
                })?;
                (user, authority)
            }
        };
        if host.is_empty() || path.is_empty() {
            polars_bail!(ComputeError: "`{url}` has no host or no path: expected ssh://[user@]host/path")
        }
        Ok(Self {
            user,
            host: host.to_owned(),
            path: format!("/{path}"),
        })
    }
}

/// An sftp file handle, read by offset through one seek-and-read at a time.
struct RemoteFile {
    /// Held so the connection outlives the handle; nothing reads it after `open`.
    _session: Session,
    _sftp: Sftp,
    file: Mutex<File>,
    len: u64,
}

impl RemoteFile {
    fn open(target: &Target, opts: &SshOpts) -> Result<Self, Box<dyn std::error::Error>> {
        let port = opts.port.unwrap_or(22);
        let stream = TcpStream::connect((target.host.as_str(), port))
            .map_err(|e| format!("connecting to {}:{port}: {e}", target.host))?;
        let mut session = Session::new()?;
        session.set_tcp_stream(stream);
        session.handshake()?;
        match &opts.identity {
            Some(identity) => session.userauth_pubkey_file(&target.user, None, identity, None)?,
            None => session.userauth_agent(&target.user)?,
        }
        let sftp = session.sftp()?;
        let mut file = sftp.open(&target.path)?;
        let len = file
            .stat()?
            .size
            .ok_or_else(|| format!("sftp stat of {} has no size", target.path))?;
        Ok(Self {
            _session: session,
            _sftp: sftp,
            file: Mutex::new(file),
            len,
        })
    }
}

impl ReadAt for RemoteFile {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        file.seek(SeekFrom::Start(offset))?;
        file.read(buf)
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.len)
    }
}
