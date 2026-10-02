//! `toon skill install`: put the skills this binary ships where an agent harness loads them.
//!
//! The skills are written in this repository (`skills/`) and compiled into the binary, so
//! what is installed always describes the commands of the `toon` that installed it.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::cli::SkillCommand;
use crate::outcome::{Error, Exit, Report};
use crate::{home, node};

/// A skill: the name of its directory, its `SKILL.md`, and the references it loads on demand,
/// each as a path relative to that directory and its text.
struct Skill {
    name: &'static str,
    body: &'static str,
    references: &'static [(&'static str, &'static str)],
}

/// Every skill the binary ships.
const SKILLS: &[Skill] = &[
    Skill {
        name: "authoring-a-nip",
        body: include_str!("../skills/authoring-a-nip/SKILL.md"),
        references: &[],
    },
    Skill {
        name: "operating-an-agent-node",
        body: include_str!("../skills/operating-an-agent-node/SKILL.md"),
        references: &[],
    },
    Skill {
        name: "social",
        body: include_str!("../skills/social/SKILL.md"),
        references: &[
            (
                "references/nip-01.md",
                include_str!("../skills/social/references/nip-01.md"),
            ),
            (
                "references/nip-02.md",
                include_str!("../skills/social/references/nip-02.md"),
            ),
            (
                "references/nip-65.md",
                include_str!("../skills/social/references/nip-65.md"),
            ),
            (
                "references/nip-51.md",
                include_str!("../skills/social/references/nip-51.md"),
            ),
            (
                "references/nip-38.md",
                include_str!("../skills/social/references/nip-38.md"),
            ),
            (
                "references/nip-58.md",
                include_str!("../skills/social/references/nip-58.md"),
            ),
            (
                "references/nip-10.md",
                include_str!("../skills/social/references/nip-10.md"),
            ),
            (
                "references/nip-22.md",
                include_str!("../skills/social/references/nip-22.md"),
            ),
            (
                "references/nip-18.md",
                include_str!("../skills/social/references/nip-18.md"),
            ),
            (
                "references/nip-25.md",
                include_str!("../skills/social/references/nip-25.md"),
            ),
            (
                "references/nip-23.md",
                include_str!("../skills/social/references/nip-23.md"),
            ),
            (
                "references/nip-88.md",
                include_str!("../skills/social/references/nip-88.md"),
            ),
            (
                "references/nip-84.md",
                include_str!("../skills/social/references/nip-84.md"),
            ),
            (
                "references/nip-28.md",
                include_str!("../skills/social/references/nip-28.md"),
            ),
            (
                "references/nip-29.md",
                include_str!("../skills/social/references/nip-29.md"),
            ),
            (
                "references/nip-72.md",
                include_str!("../skills/social/references/nip-72.md"),
            ),
            (
                "references/nip-17.md",
                include_str!("../skills/social/references/nip-17.md"),
            ),
            (
                "references/nip-09.md",
                include_str!("../skills/social/references/nip-09.md"),
            ),
            (
                "references/nip-40.md",
                include_str!("../skills/social/references/nip-40.md"),
            ),
            (
                "references/nip-56.md",
                include_str!("../skills/social/references/nip-56.md"),
            ),
            (
                "references/nip-36.md",
                include_str!("../skills/social/references/nip-36.md"),
            ),
        ],
    },
];

pub fn run(command: SkillCommand) -> Result<Report, Error> {
    match command {
        SkillCommand::Install { dir } => install(dir.as_deref()),
    }
}

/// Where an agent harness (Claude Code) loads skills from: `~/.claude/skills`.
fn default_directory() -> Result<PathBuf, Error> {
    Ok(home::user()?.join(".claude").join("skills"))
}

/// `toon skill install`: write each shipped skill to `<directory>/<name>/SKILL.md`, with its references beside it,
/// replacing the one an earlier release installed. Running it again changes nothing.
fn install(directory: Option<&Path>) -> Result<Report, Error> {
    let directory = match directory {
        Some(directory) => directory.to_owned(),
        None => default_directory()?,
    };
    let mut installed = Vec::new();
    for skill in SKILLS {
        let file = directory.join(skill.name).join("SKILL.md");
        // Written in full or not at all, so a harness never reads half a file.
        node::write(&file, skill.body.as_bytes(), 0o644)?;
        for (path, text) in skill.references {
            node::write(
                &directory.join(skill.name).join(path),
                text.as_bytes(),
                0o644,
            )?;
        }
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
