//! The `systemd --user` unit that keeps the supervisor running across logins, crashes
//! and reboots.
//!
//! `toon up` writes the unit under `~/.config/systemd/user`, and then asks `systemctl`
//! to load and start it. The unit runs `toon up --foreground`, so the supervisor is the
//! same process whoever starts it. `toon down` disables and stops the unit.

use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::home;
use crate::node;
use crate::outcome::{Error, ErrorCode};

/// The unit's name. One supervisor runs per machine, so there is one unit.
pub const UNIT: &str = "toon-agent-node.service";

/// Where the unit is written: where `systemd --user` looks for the user's own units,
/// `$XDG_CONFIG_HOME/systemd/user`, which is `~/.config/systemd/user` by default.
pub fn unit_path(user_home: &Path) -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|config| config.is_absolute())
        .unwrap_or_else(|| user_home.join(".config"));
    config.join("systemd").join("user").join(UNIT)
}

/// A word of an `ExecStart=` line, quoted the way systemd reads it.
fn quoted(word: &str) -> String {
    let escaped = word
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    format!("\"{escaped}\"")
}

/// Tell systemd that the supervisor answers on its control socket, when systemd is
/// waiting for that (`NOTIFY_SOCKET` is set). Without `NOTIFY_SOCKET` nothing is sent,
/// and a notice that cannot be sent is not an error: the supervisor runs either way.
pub fn notify_ready() {
    let Some(target) = std::env::var_os("NOTIFY_SOCKET").filter(|target| !target.is_empty()) else {
        return;
    };
    let Ok(socket) = UnixDatagram::unbound() else {
        return;
    };
    let message = b"READY=1";
    let bytes = target.as_bytes();
    #[cfg(target_os = "linux")]
    if let Some(name) = bytes.strip_prefix(b"@") {
        // An abstract-namespace socket: the `@` stands for a leading NUL byte.
        use std::os::linux::net::SocketAddrExt;
        if let Ok(address) = SocketAddr::from_abstract_name(name) {
            let _ = socket.send_to_addr(message, &address);
        }
        return;
    }
    let _ = socket.send_to(message, Path::new(&target));
}

/// The unit that runs the supervisor of the agent node, started from the binary `toon`.
///
/// `TimeoutStartSec` is 600 seconds: a first start may download the `anon` overlay and
/// pull app images, which can outlast systemd's 90 second default on a slow link.
pub fn unit(toon: &Path) -> String {
    format!(
        "# Written by `toon up`. `toon up` overwrites it and `toon down` stops it.\n\
         [Unit]\n\
         Description=TOON agent node\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         StartLimitIntervalSec=120\n\
         StartLimitBurst=5\n\
         \n\
         [Service]\n\
         Type=notify\n\
         TimeoutStartSec=600\n\
         ExecStart={} up --foreground\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        quoted(&toon.to_string_lossy()),
    )
}

fn failed(message: String) -> Error {
    Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::SystemdFailed,
        message,
    }
}

fn run(program: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("`{program}` could not be run: {error}."))
}

fn systemctl(args: &[&str]) -> Result<(), Error> {
    let mut all = vec!["--user"];
    all.extend_from_slice(args);
    let output = run("systemctl", &all).map_err(failed)?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    Err(failed(format!(
        "`systemctl --user {}` failed ({}): {}",
        args.join(" "),
        output.status,
        said.trim()
    )))
}

/// The last lines the supervisor wrote, for the message of a start that failed. Empty
/// when `journalctl` cannot say.
fn journal_tail() -> String {
    let Ok(output) = run(
        "journalctl",
        &["--user", "-u", UNIT, "-n", "5", "--no-pager", "-o", "cat"],
    ) else {
        return String::new();
    };
    let said = String::from_utf8_lossy(&output.stdout);
    let said = said.trim();
    if !output.status.success() || said.is_empty() {
        return String::new();
    }
    format!(" The supervisor said:\n{said}")
}

/// What installing the unit did.
pub struct Installed {
    pub unit: PathBuf,
    /// Whether the user's manager will start at boot, before anyone logs in.
    pub linger: bool,
}

/// Write the unit, then load, enable and start it. The unit is on disk even when
/// `systemctl` fails, so that what would have run can be read.
pub fn install() -> Result<Installed, Error> {
    let user_home = home::user()?;
    let toon = std::env::current_exe().map_err(|error| Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::Io,
        message: format!("This binary could not find itself: {error}."),
    })?;
    let path = unit_path(&user_home);
    node::write(&path, unit(&toon).as_bytes(), 0o644)?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", UNIT]).map_err(|mut error| {
        error.message.push_str(&journal_tail());
        error
    })?;
    // Without lingering the user's manager, and so the supervisor, starts at the first
    // login and not at boot. It may need privileges this user does not have.
    let linger = run("loginctl", &["enable-linger"]).is_ok_and(|output| output.status.success());
    Ok(Installed { unit: path, linger })
}

/// Stop the unit and keep it from starting again at boot, if `up` ever installed it.
/// A node that never had a unit has nothing to stop here; a unit that `systemctl` would
/// not stop is an error, because it would bring the agent node back.
pub fn remove() -> Result<(), Error> {
    if unit_path(&home::user()?).exists() {
        systemctl(&["disable", "--now", UNIT])?;
    }
    Ok(())
}
