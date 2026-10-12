//! The variables and secrets an operator gives an app when adding it (ADR 0012).
//!
//! A variable is kept in the app's record. A secret is copied into the app's `secrets`
//! directory in the home, outside its data directory, in files only the operator's account
//! can read; the record holds its name or its path in the container and never its contents.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use std::io::Write;

use crate::apps::Origin;
use crate::cli::SecretArgs;
use crate::node::{App, AppFiles};
use crate::outcome::{Error, ErrorCode};
use crate::runner::Allowances;

/// The most a secret's file may hold.
const LARGEST: u64 = 64 * 1024;

fn refused(message: String) -> Error {
    Error {
        code: ErrorCode::Usage,
        message,
        nothing_sent: true,
        unanswered: None,
    }
}

/// What an operator gave an app, with the contents of the files read.
#[derive(Clone, Default)]
pub struct Given {
    pub variables: BTreeMap<String, String>,
    /// The secrets given as variables, by name, with the value the app is started with.
    pub secret_variables: Vec<(String, String)>,
    /// The secrets given as files, by path in the container, with the bytes mounted there.
    pub secret_files: Vec<(String, Vec<u8>)>,
}

impl std::fmt::Debug for Given {
    /// Never the contents of a secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Given")
            .field("variables", &self.variables)
            .field(
                "secret_variables",
                &self
                    .secret_variables
                    .iter()
                    .map(|(n, _)| n)
                    .collect::<Vec<_>>(),
            )
            .field(
                "secret_files",
                &self.secret_files.iter().map(|(p, _)| p).collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// The name and the rest of `NAME=rest`.
fn split<'a>(flag: &str, text: &'a str) -> Result<(&'a str, &'a str), Error> {
    text.split_once('=')
        .ok_or_else(|| refused(format!("{flag} {text}: it must be NAME=value.")))
}

fn check_name(flag: &str, name: &str) -> Result<(), Error> {
    let mut chars = name.chars();
    let valid = chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        return Err(refused(format!(
            "{flag} {name}: a name is a letter or `_`, then letters, digits and `_`."
        )));
    }
    if name.starts_with("TOON_") {
        return Err(refused(format!(
            "{flag} {name}: names that begin `TOON_` belong to the contract between `toon` and an app."
        )));
    }
    Ok(())
}

/// The bytes of the operator's `file`, if it can be read, is not empty and is not too large.
fn read(flag: &str, file: &str) -> Result<Vec<u8>, Error> {
    let why = |why: String| refused(format!("{flag} {file}: {why}."));
    let mut bytes = Vec::new();
    fs::File::open(file)
        .and_then(|opened| opened.take(LARGEST + 1).read_to_end(&mut bytes))
        .map_err(|error| why(error.to_string()))?;
    if bytes.is_empty() {
        return Err(why("the file is empty".into()));
    }
    if bytes.len() as u64 > LARGEST {
        return Err(why(format!(
            "the file is larger than {} KiB",
            LARGEST / 1024
        )));
    }
    Ok(bytes)
}

fn variable_value(file: &str, mut bytes: Vec<u8>) -> Result<String, Error> {
    if bytes.ends_with(b"\r\n") {
        bytes.truncate(bytes.len() - 2);
    } else if bytes.ends_with(b"\n") {
        bytes.truncate(bytes.len() - 1);
    }
    let why = |why: &str| refused(format!("--secret {file}: {why}."));
    let value = String::from_utf8(bytes).map_err(|_| why("the contents are not UTF-8"))?;
    if value.contains('\0') {
        return Err(why("the contents hold a NUL byte"));
    }
    if value.contains('\n') {
        return Err(why("the contents hold a newline other than the last"));
    }
    Ok(value)
}

fn check_path(path: &str, allowances: &Allowances) -> Result<(), Error> {
    let why = |why: &str| refused(format!("--secret-file {path}: {why}."));
    if !path.starts_with('/') {
        return Err(why("it must be an absolute path in the container"));
    }
    if path.contains([':', ',', '\n', '\0']) || path.split('/').any(|part| part == "..") {
        return Err(why("it must not hold `:`, `,`, a newline or `..`"));
    }
    let bare = path.trim_end_matches('/');
    let under = |dir: &str| bare == dir || bare.starts_with(&format!("{dir}/"));
    if bare.is_empty()
        || under("/data")
        || under("/tmp")
        || allowances.scratch.iter().any(|scratch| under(scratch))
    {
        return Err(why(
            "the app writes to `/`, `/data`, `/tmp` and its scratch paths, so it cannot be mounted at or under them",
        ));
    }
    Ok(())
}

impl Given {
    /// What the flags of `toon add` and `toon create` give, or `usage` if one cannot be used.
    /// Nothing is written.
    pub fn parse(
        origin: &Origin,
        flags: &SecretArgs,
        allowances: &Allowances,
    ) -> Result<Self, Error> {
        let mut given = Self::default();
        if flags.env.is_empty() && flags.secret.is_empty() && flags.secret_file.is_empty() {
            return Ok(given);
        }
        if matches!(origin, Origin::Url(_)) {
            return Err(refused(
                "--env, --secret and --secret-file give an app that runs from an image \
                 something to start with; an app at a --url is not run."
                    .into(),
            ));
        }
        let mut names: Vec<String> = Vec::new();
        let mut take = |flag: &str, name: &str| -> Result<(), Error> {
            check_name(flag, name)?;
            if names.iter().any(|known| known == name) {
                return Err(refused(format!("{flag} {name}: the name is given twice.")));
            }
            names.push(name.to_owned());
            Ok(())
        };
        for text in &flags.env {
            let (name, value) = split("--env", text)?;
            take("--env", name)?;
            given.variables.insert(name.to_owned(), value.to_owned());
        }
        for text in &flags.secret {
            let (name, file) = split("--secret", text)?;
            take("--secret", name)?;
            let value = variable_value(file, read("--secret", file)?)?;
            given.secret_variables.push((name.to_owned(), value));
        }
        for text in &flags.secret_file {
            let (path, file) = split("--secret-file", text)?;
            check_path(path, allowances)?;
            if given.secret_files.iter().any(|(known, _)| known == path) {
                return Err(refused(format!(
                    "--secret-file {path}: the path is given twice."
                )));
            }
            given
                .secret_files
                .push((path.to_owned(), read("--secret-file", file)?));
        }
        Ok(given)
    }

    /// Put what was given in the record of `app`.
    pub fn record(&self, app: &mut App) {
        app.variables = self.variables.clone();
        app.secret_variables = self
            .secret_variables
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        app.secret_files = self.secret_files.iter().map(|(p, _)| p.clone()).collect();
    }

    /// Copy the secrets into the home, for the app `name`: a directory only the operator's
    /// account can enter, files only it can read. Nothing is kept if it fails.
    pub fn keep(&self, home: &Path, name: &str) -> Result<(), Error> {
        if self.secret_variables.is_empty() && self.secret_files.is_empty() {
            return Ok(());
        }
        let dir = AppFiles::of(home, name).secrets_dir;
        let result = (|| {
            let _ = fs::remove_dir_all(&dir);
            if let Some(parent) = dir.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::DirBuilder::new().mode(0o700).create(&dir)?;
            for (variable, value) in &self.secret_variables {
                write_private(&variable_copy(&dir, variable), value.as_bytes())?;
            }
            for (index, (_, bytes)) in self.secret_files.iter().enumerate() {
                write_private(&file_copy(&dir, index), bytes)?;
            }
            Ok::<(), std::io::Error>(())
        })();
        result.map_err(|error| {
            forget(home, name);
            Error {
                code: ErrorCode::Io,
                message: format!(
                    "The secrets could not be kept in {}: {error}.",
                    dir.display()
                ),
                nothing_sent: false,
                unanswered: None,
            }
        })
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

fn variable_copy(dir: &Path, variable: &str) -> PathBuf {
    dir.join(format!("variable-{variable}"))
}

fn file_copy(dir: &Path, index: usize) -> PathBuf {
    dir.join(format!("file-{index}"))
}

/// Remove the copies of the secrets of the app `name`.
pub fn forget(home: &Path, name: &str) {
    let _ = fs::remove_dir_all(AppFiles::of(home, name).secrets_dir);
}

/// The copies of secrets and the paths they are mounted at.
pub type Mounts = Vec<(PathBuf, String)>;

/// What `app` is started with beyond what `toon` gives every app: its variables and the
/// values of its secrets given as variables, and its secrets given as files as the copy in
/// the home and the path in the container.
pub fn for_start(home: &Path, app: &App) -> Result<(Vec<(String, String)>, Mounts), Error> {
    let dir = AppFiles::of(home, &app.name).secrets_dir;
    let missing = |what: &str| Error {
        code: ErrorCode::AppFailed,
        message: format!(
            "A secret of the app {} ({what}) is not readable in {}.",
            app.name,
            dir.display()
        ),
        nothing_sent: false,
        unanswered: None,
    };
    let mut env: Vec<(String, String)> = app
        .variables
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    for name in &app.secret_variables {
        let value = fs::read_to_string(variable_copy(&dir, name)).map_err(|_| missing(name))?;
        env.push((name.clone(), value));
    }
    let mut mounts = Vec::new();
    for (index, path) in app.secret_files.iter().enumerate() {
        let copy = file_copy(&dir, index);
        if !copy.is_file() {
            return Err(missing(path));
        }
        mounts.push((copy, path.clone()));
    }
    Ok((env, mounts))
}
