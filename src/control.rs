//! The supervisor's control socket: how `status` and `down` talk to `up`.
//!
//! A Unix socket in the agent node's home. A client connects, writes one line of JSON
//! naming a request, and reads one line of JSON back.

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

/// How long either side waits on the other before it gives up on the connection.
const PATIENCE: Duration = Duration::from_secs(5);

pub fn path(home: &Path) -> PathBuf {
    home.join("supervisor.sock")
}

/// Ask the supervisor of the agent node at `home` a question. `None` if no supervisor
/// answers: there is none, or the socket is a dead one's.
pub fn ask(home: &Path, request: &str) -> Option<Value> {
    ask_within(home, request, PATIENCE)
}

/// `ask`, for a request that takes longer than `PATIENCE` to answer.
pub fn ask_within(home: &Path, request: &str, patience: Duration) -> Option<Value> {
    let mut stream = UnixStream::connect(path(home)).ok()?;
    stream.set_read_timeout(Some(patience)).ok()?;
    stream.set_write_timeout(Some(PATIENCE)).ok()?;
    writeln!(stream, "{}", json!({ "request": request })).ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

/// Whether a supervisor is answering at `home`.
pub fn running(home: &Path) -> bool {
    UnixStream::connect(path(home)).is_ok()
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
    UnixListener::bind(&socket).map(Some)
}

/// Answer requests, one connection at a time, until the process ends. `answer` gets the
/// request's name and returns the reply.
pub fn serve(listener: UnixListener, answer: impl Fn(&str) -> Value) {
    for stream in listener.incoming().flatten() {
        let _ = stream.set_read_timeout(Some(PATIENCE));
        let _ = stream.set_write_timeout(Some(PATIENCE));
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }
        let request: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let reply = match request["request"].as_str() {
            Some(name) => answer(name),
            None => json!({ "error": "not a request" }),
        };
        let mut stream = stream;
        let _ = writeln!(stream, "{reply}");
        let _ = stream.shutdown(Shutdown::Both);
    }
}
