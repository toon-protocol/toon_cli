//! The list of trusted images (ADR 0010): the images an agent key may add at once.
//!
//! The home holds it as `trusted-images`, a text file with one entry to a line. An entry is
//! the repository of an image, such as `ghcr.io/toon-protocol/echo`, or a prefix ending in
//! `/*`, such as `ghcr.io/toon-protocol/*`, which matches every repository below it. Blank
//! lines and lines that start with `#` are ignored, and so is a line that is neither form. An
//! image matches by its repository, without its tag or digest. `toon init` writes the file
//! seeded with `ghcr.io/toon-protocol/*`; an operator edits it by hand. A home without the
//! file trusts that seed, and an empty file trusts nothing.
//!
//! Only an agent key's add is checked against the list, to decide whether it is added at
//! once. An operator's add is never refused by it: the one thing the list does to an operator
//! is to decide whether the node's runtime applies to the app (ADR 0011). The node's runtime
//! is the first line of `untrusted-runtime` that is not blank and does not start with `#`.

use std::path::{Path, PathBuf};

use crate::outcome::{Error, ErrorCode};

/// What the list holds in a home that has been given none.
pub const SEED: &str = "ghcr.io/toon-protocol/*";

const FILE_NAME: &str = "trusted-images";
const RUNTIME_FILE_NAME: &str = "untrusted-runtime";

pub fn path(home: &Path) -> PathBuf {
    home.join(FILE_NAME)
}

/// Write the seeded list unless the home has a list already. The home is not changed
/// otherwise, so that a list the operator edited is never overwritten.
pub fn seed(home: &Path) -> std::io::Result<()> {
    let file = path(home);
    if file.exists() {
        return Ok(());
    }
    std::fs::write(
        file,
        format!(
            "# Images an agent key may add at once, one to a line: a repository, or a prefix\n\
             # ending in /* for every repository below it. Edit this file to change the list.\n\
             {SEED}\n"
        ),
    )
}

/// An image reference without its digest and its tag.
pub fn repository(image: &str) -> &str {
    let image = image.split_once('@').map_or(image, |(name, _)| name);
    match image.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => name,
        _ => image,
    }
}

/// Whether `entry` names `repository`.
fn names(entry: &str, repository: &str) -> bool {
    match entry.strip_suffix("/*") {
        Some(prefix) => {
            !prefix.is_empty()
                && repository.len() > prefix.len() + 1
                && repository.starts_with(prefix)
                && repository[prefix.len()..].starts_with('/')
        }
        None => !entry.contains('*') && entry == repository,
    }
}

/// A repository that is a plain path of names: nothing that could be read as a way out of
/// the prefix an entry names.
fn plain(repository: &str) -> bool {
    !repository.is_empty()
        && !repository
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
        && repository
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Whether the list in `text` trusts `image`.
fn trusts(text: &str, image: &str) -> bool {
    let repository = repository(image);
    plain(repository)
        && text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .any(|entry| names(entry, repository))
}

/// Whether the home's list trusts `image`. A list that cannot be read trusts nothing and is
/// an error, so that an add is refused rather than let through.
pub fn is_trusted(home: &Path, image: &str) -> Result<bool, Error> {
    let file = path(home);
    match std::fs::read_to_string(&file) {
        Ok(text) => Ok(trusts(&text, image)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(trusts(SEED, image)),
        Err(source) => Err(Error {
            code: ErrorCode::Io,
            message: format!("{}: {source}.", file.display()),
            nothing_sent: false,
            unanswered: None,
        }),
    }
}

/// The runtime the home names for every image off the list, or `None` if the file is not
/// there or names none. `toon init` never writes it. A name docker cannot take, such as one
/// it would read as a flag, is `usage`.
pub fn untrusted_runtime(home: &Path) -> Result<Option<String>, Error> {
    let file = home.join(RUNTIME_FILE_NAME);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error {
                code: ErrorCode::Io,
                message: format!("{}: {source}.", file.display()),
                nothing_sent: false,
                unanswered: None,
            })
        }
    };
    let Some(name) = first_name(&text) else {
        return Ok(None);
    };
    crate::runner::check_runtime(name)
        .map(|()| Some(name.to_owned()))
        .map_err(|mut error| {
            error.message = format!("{}: {}", file.display(), error.message);
            error
        })
}

/// The first line of `text` that is not blank and does not start with `#`.
fn first_name(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nodes_runtime_is_the_first_line_that_names_one() {
        assert_eq!(first_name("# gVisor\n\n  runsc \nkata\n"), Some("runsc"));
        assert_eq!(first_name("# only a comment\n\n"), None);
        assert_eq!(first_name(""), None);
    }

    #[test]
    fn an_image_matches_by_its_repository() {
        let list = "# a comment\n\nghcr.io/toon-protocol/*\nexample.com:5000/team/app\n";
        for image in [
            "ghcr.io/toon-protocol/echo",
            "ghcr.io/toon-protocol/echo:1.2",
            "ghcr.io/toon-protocol/echo@sha256:abcd",
            "ghcr.io/toon-protocol/echo:1@sha256:abcd",
            "ghcr.io/toon-protocol/team/echo",
            "example.com:5000/team/app:3",
        ] {
            assert!(trusts(list, image), "{image}");
        }
        for image in [
            "ghcr.io/toon-protocol",
            "ghcr.io/toon-protocol/",
            "ghcr.io/toon-protocolx/echo",
            "ghcr.io/other/echo",
            "ghcr.io/toon-protocol/../other/echo",
            "ghcr.io/toon-protocol//echo",
            "example.com:5000/team/app2",
            "example.com:5000/team",
            "echo",
        ] {
            assert!(!trusts(list, image), "{image}");
        }
    }

    #[test]
    fn a_line_that_is_neither_form_trusts_nothing() {
        assert!(!trusts("*\n/*\nghcr.io/*x\n", "ghcr.io/toon-protocol/echo"));
        assert!(!trusts("", "ghcr.io/toon-protocol/echo"));
    }
}
