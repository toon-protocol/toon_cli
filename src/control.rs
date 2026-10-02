//! The supervisor's control socket: how `status` and `down` talk to `up`.
//!
//! A Unix socket in the agent node's home. A client connects, writes one line of JSON
//! naming a request, and reads one line of JSON back.

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

/// How long either side waits on the other before it gives up on the connection.
const PATIENCE: Duration = Duration::from_secs(5);

/// The socket's file name in the agent node's home.
const FILE_NAME: &str = "supervisor.sock";

pub fn path(home: &Path) -> PathBuf {
    home.join(FILE_NAME)
}

/// Run `use_address` with a path to the socket that is short whatever `home` is: a Unix
/// socket address holds at most 107 bytes, and a home can be longer. The home is opened as
/// a directory (close-on-exec, so no child inherits it) and the socket is addressed through
/// `/proc/self/fd`; the descriptor lives only for the call.
fn through_short_path<T>(
    home: &Path,
    use_address: impl FnOnce(&Path) -> std::io::Result<T>,
) -> std::io::Result<T> {
    let directory = std::fs::File::open(home)?;
    let address = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(FILE_NAME);
    use_address(&address)
}

fn connect(home: &Path) -> std::io::Result<UnixStream> {
    through_short_path(home, |address| UnixStream::connect(address))
}

/// Ask the supervisor of the agent node at `home` a question. `None` if no supervisor
/// answers: there is none, or the socket is a dead one's.
pub fn ask(home: &Path, request: &str) -> Option<Value> {
    ask_within(home, request, PATIENCE)
}

/// `ask`, for a request that takes longer than `PATIENCE` to answer.
pub fn ask_within(home: &Path, request: &str, patience: Duration) -> Option<Value> {
    ask_about(home, request, None, patience)
}

/// `ask_within`, for a request that is about one TOON app, named `toon_app`.
pub fn ask_about(
    home: &Path,
    request: &str,
    toon_app: Option<&str>,
    patience: Duration,
) -> Option<Value> {
    let mut stream = connect(home).ok()?;
    stream.set_read_timeout(Some(patience)).ok()?;
    stream.set_write_timeout(Some(PATIENCE)).ok()?;
    writeln!(
        stream,
        "{}",
        json!({ "request": request, "toon_app": toon_app })
    )
    .ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

/// Whether a supervisor is answering at `home`.
pub fn running(home: &Path) -> bool {
    connect(home).is_ok()
}

/// Bind the socket. A socket left by a supervisor that is gone is replaced; one that
/// something is listening on is not, and `None` says another supervisor has the node.
pub fn bind(home: &Path) -> std::io::Result<Option<UnixListener>> {
    let socket = path(home);
    if running(home) {
        return Ok(None);
    }
    match std::fs::remove_file(&socket) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    through_short_path(home, |address| UnixListener::bind(address)).map(Some)
}

/// Answer requests until the process ends, each connection on its own thread, so that a
/// request that takes long to answer, such as `reload`, does not hold up `status` or
/// `down`. `answer` gets the request's name and the TOON app it is about, if it names one,
/// and returns the reply.
pub fn serve(
    listener: UnixListener,
    answer: impl Fn(&str, Option<&str>) -> Value + Send + Sync + 'static,
) {
    let answer = Arc::new(answer);
    for stream in listener.incoming().flatten() {
        let answer = Arc::clone(&answer);
        thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(PATIENCE));
            let _ = stream.set_write_timeout(Some(PATIENCE));
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                return;
            }
            let request: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            let reply = match request["request"].as_str() {
                Some(name) => answer(name, request["toon_app"].as_str()),
                None => json!({ "error": "not a request" }),
            };
            let mut stream = stream;
            let _ = writeln!(stream, "{reply}");
            let _ = stream.shutdown(Shutdown::Both);
        });
    }
}
