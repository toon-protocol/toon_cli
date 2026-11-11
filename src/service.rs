//! The `systemd --user` unit that keeps the supervisor running across logins, crashes
//! and reboots.
//!
//! `toon up` writes the unit under `~/.config/systemd/user`, and then asks `systemctl`
//! to load and start it. The unit runs `toon up --foreground`, so the supervisor is the
//! same process whoever starts it. `toon down` disables and stops the unit.
//!
//! The one unit belongs to one home: it carries `Environment=HOME=`, so the supervisor
//! systemd starts runs against the home that installed it. `toon up` and `toon down`
//! refuse to take over or stop a unit that records another home. A unit that records
//! none, written by an older `toon`, belongs to the login home.

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::home;
use crate::node;
use crate::outcome::{Error, ErrorCode};

/// The unit's name. One supervisor runs per machine, so there is one unit, and it
/// belongs to one home: the one that installed it.
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

/// The `Environment=` line that gives the supervisor its home. The assignment is bare
/// when no character needs quoting, and quoted as a whole otherwise. Unlike `ExecStart=`,
/// systemd expands no `$` in `Environment=`, only `%` specifiers.
fn home_line(home: &Path) -> String {
    let assignment = format!("HOME={}", home.to_string_lossy());
    let plain = assignment
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+@:,=".contains(c));
    if plain {
        format!("Environment={assignment}")
    } else {
        let escaped = assignment
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%");
        format!("Environment=\"{escaped}\"")
    }
}

/// The inverse of `home_line`, for the value of an `Environment=` line.
fn unquoted(value: &str) -> String {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return value.to_string();
    };
    let mut out = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        let escaped = matches!(
            (c, chars.peek()),
            ('\\', Some('\\')) | ('\\', Some('"')) | ('%', Some('%'))
        );
        if escaped {
            out.push(chars.next().unwrap_or(c));
        } else {
            out.push(c);
        }
    }
    out
}

/// The home a unit's text records, or `None` for a unit that records none.
fn recorded_home(unit: &str) -> Option<PathBuf> {
    unit.lines()
        .filter_map(|line| line.strip_prefix("Environment="))
        .find_map(|value| {
            unquoted(value.trim())
                .strip_prefix("HOME=")
                .map(PathBuf::from)
        })
}

/// The login home of the user running this process, from the passwd entry and not from
/// `$HOME`: `/etc/passwd`, else `getent`, which also asks NSS (LDAP, sssd). `None` when
/// neither has the user.
fn login_home() -> Option<PathBuf> {
    let uid = fs::metadata("/no-proc/self").ok()?.uid().to_string();
    let home = |passwd: &str| {
        passwd.lines().find_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            (fields.len() >= 6 && fields[2] == uid && !fields[5].is_empty())
                .then(|| PathBuf::from(fields[5]))
        })
    };
    fs::read_to_string("/etc/passwd")
        .ok()
        .and_then(|passwd| home(&passwd))
        .or_else(|| {
            let output = Command::new("getent")
                .args(["passwd", &uid])
                .output()
                .ok()?;
            home(&String::from_utf8_lossy(&output.stdout))
        })
}

fn same_home(a: &Path, b: &Path) -> bool {
    a == b || matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// The home that owns a unit when it is not `home`, or `None` when `home` owns it. A
/// unit that records no home belongs to the login home, which `login` finds; when it
/// cannot be found the unit is not held against `home`.
fn other_owner(
    unit: &str,
    home: &Path,
    login: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    let owner = recorded_home(unit).or_else(login)?;
    (!same_home(&owner, home)).then_some(owner)
}

/// Refuse when the unit already at `path` belongs to a home other than `home`.
fn check_owner(path: &Path, home: &Path) -> Result<(), Error> {
    let Ok(existing) = fs::read_to_string(path) else {
        return Ok(());
    };
    match other_owner(&existing, home, login_home) {
        None => Ok(()),
        Some(owner) => Err(Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::UnitOwnedByAnotherHome,
            message: format!(
                "The systemd unit at {} belongs to another home, {}, and there is one unit per machine, so `toon` leaves it alone. Run `toon up --foreground` to run this home without systemd.",
                path.display(),
                owner.display()
            ),
        }),
    }
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
/// `home` is the user's home that installs it, recorded as `Environment=HOME=`.
///
/// `TimeoutStartSec` is 600 seconds: a first start may download the `anon` overlay and
/// pull app images, which can outlast systemd's 90 second default on a slow link.
pub fn unit(toon: &Path, home: &Path) -> String {
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
         {}\n\
         ExecStart={} up --foreground\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        home_line(home),
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
    check_owner(&path, &user_home)?;
    node::write(&path, unit(&toon, &user_home).as_bytes(), 0o644)?;
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
/// not stop is an error, because it would bring the agent node back, and so is a unit
/// that another home installed, which is left running.
pub fn remove() -> Result<(), Error> {
    let user_home = home::user()?;
    let path = unit_path(&user_home);
    if path.exists() {
        check_owner(&path, &user_home)?;
        systemctl(&["disable", "--now", UNIT])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unit_records_its_home_and_reads_it_back() {
        for home in ["/home/a", "/home/with space", "/h/\"q\"%$\\"] {
            let text = unit(Path::new("/bin/toon"), Path::new(home));
            assert_eq!(recorded_home(&text), Some(PathBuf::from(home)), "{text}");
        }
        assert!(unit(Path::new("/bin/toon"), Path::new("/home/a"))
            .contains("Environment=HOME=/home/a\n"));
        assert!(unit(Path::new("/bin/toon"), Path::new("/home/$a %b"))
            .contains("Environment=\"HOME=/home/$a %%b\"\n"));
    }

    #[test]
    fn a_legacy_unit_belongs_to_the_login_home() {
        let legacy = "[Service]\nExecStart=\"/bin/toon\" up --foreground\n";
        assert_eq!(recorded_home(legacy), None);
        let login = || Some(PathBuf::from("/home/login"));
        assert_eq!(other_owner(legacy, Path::new("/home/login"), login), None);
        assert_eq!(
            other_owner(legacy, Path::new("/tmp/scratch"), login),
            Some(PathBuf::from("/home/login"))
        );
        assert_eq!(
            other_owner(legacy, Path::new("/tmp/scratch"), || None),
            None
        );
    }

    #[test]
    fn a_unit_for_another_home_is_refused() {
        let text = unit(Path::new("/bin/toon"), Path::new("/home/a"));
        assert_eq!(other_owner(&text, Path::new("/home/a"), || None), None);
        assert_eq!(
            other_owner(&text, Path::new("/home/b"), || None),
            Some(PathBuf::from("/home/a"))
        );
    }
}
