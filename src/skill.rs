//! `toon skill install`: put the skills this binary ships where an agent harness loads them.
//!
//! The skills are written in this repository (`skills/`) and compiled into the binary, so
//! what is installed always describes the commands of the `toon` that installed it.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::home;
use crate::outcome::{Error, ErrorCode, Exit, Report};

/// A skill: the name of its directory, and its `SKILL.md`.
pub struct Skill {
    pub name: &'static str,
    pub body: &'static str,
}

/// Every skill the binary ships.
pub const SKILLS: &[Skill] = &[Skill {
    name: "operating-an-agent-node",
    body: include_str!("../skills/operating-an-agent-node/SKILL.md"),
}];

fn io(path: &Path, error: std::io::Error) -> Error {
    Error {
        code: ErrorCode::Io,
        message: format!("{} could not be written: {error}.", path.display()),
    }
}

/// Where an agent harness (Claude Code) loads skills from: `~/.claude/skills`.
fn default_directory() -> Result<PathBuf, Error> {
    Ok(home::user()?.join(".claude").join("skills"))
}

/// `toon skill install`: write each shipped skill to `<directory>/<name>/SKILL.md`,
/// replacing the one an earlier release installed. Running it again changes nothing.
pub fn install(directory: Option<&Path>) -> Result<Report, Error> {
    let directory = match directory {
        Some(directory) => directory.to_owned(),
        None => default_directory()?,
    };
    let mut installed = Vec::new();
    for skill in SKILLS {
        let folder = directory.join(skill.name);
        fs::create_dir_all(&folder).map_err(|error| io(&folder, error))?;
        let file = folder.join("SKILL.md");
        // Written beside the target and renamed over it, so a harness never reads half a file.
        let partial = folder.join(".SKILL.md.partial");
        fs::write(&partial, skill.body).map_err(|error| io(&partial, error))?;
        fs::rename(&partial, &file).map_err(|error| io(&file, error))?;
        installed.push((skill.name, file));
    }
    let text = installed
        .iter()
        .map(|(name, file)| format!("Installed {name} at {}.", file.display()))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "directory": directory,
            "skills": installed
                .iter()
                .map(|(name, file)| json!({ "name": name, "path": file }))
                .collect::<Vec<_>>(),
        }),
        text,
    })
}
