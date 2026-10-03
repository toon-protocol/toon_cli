mod support;

use std::fs;
use std::path::{Path, PathBuf};

use support::Machine;

/// The `SKILL.md` of the skill `name` in `skills/`.
fn skill_text(name: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("skills/{name}/SKILL.md")),
    )
    .expect("read the skill")
}

/// The skills in `skills/`: each directory's name and its `SKILL.md`.
fn shipped_skills() -> Vec<(String, PathBuf)> {
    let mut skills: Vec<(String, PathBuf)> =
        fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("skills"))
            .expect("read skills/")
            .map(|entry| entry.expect("read skills/").path())
            .filter(|path| path.is_dir())
            .map(|path| {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                (name, path.join("SKILL.md"))
            })
            .collect();
    skills.sort();
    skills
}

/// Every Markdown file of a shipped skill, as a path relative to its directory: `SKILL.md` and
/// the references beside it.
fn skill_files(dir: &Path) -> Vec<String> {
    let mut files = vec!["SKILL.md".to_owned()];
    if let Ok(entries) = fs::read_dir(dir.join("references")) {
        files.extend(entries.map(|entry| {
            format!(
                "references/{}",
                entry
                    .expect("read references/")
                    .file_name()
                    .to_string_lossy()
            )
        }));
    }
    files.sort();
    files
}

/// Every command a skill file names, as its words: an inline code span that starts with
/// `toon `, and a line of a code block that does.
fn named_commands(text: &str) -> Vec<Vec<String>> {
    let spans = text
        .split('`')
        .skip(1)
        .step_by(2)
        .filter_map(|span| span.strip_prefix("toon "));
    let lines = text
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("toon "));
    spans
        .chain(lines)
        .map(|rest| rest.split_whitespace().map(str::to_owned).collect())
        .collect()
}

#[test]
fn every_command_the_skill_names_exists_in_the_binary() {
    let machine = Machine::new();
    assert!(
        named_commands(&skill_text("operating-an-agent-node")).len() > 30,
        "the skill names its commands"
    );
    let mut commands: Vec<Vec<String>> = shipped_skills()
        .into_iter()
        .flat_map(|(_, file)| {
            let dir = file.parent().unwrap().to_owned();
            skill_files(&dir).into_iter().flat_map(move |relative| {
                named_commands(&fs::read_to_string(dir.join(relative)).expect("read a skill"))
            })
        })
        .collect();
    commands.sort();
    commands.dedup();
    assert!(
        commands
            .iter()
            .filter(|words| words.starts_with(&["event".to_owned()]))
            .count()
            > 40,
        "the social references show their commands"
    );
    for words in commands {
        // The command is the leading plain words; a value is `<placeholder>`, and a flag
        // starts with `--`.
        let path: Vec<&str> = words
            .iter()
            .map(String::as_str)
            .take_while(|word| {
                word.chars().all(|c| c.is_ascii_lowercase() || c == '-') && !word.starts_with('-')
            })
            .collect();
        let mut args = path.clone();
        args.push("--help");
        let help = machine.toon(&args);
        assert_eq!(
            help.exit_code,
            0,
            "`toon {}` is not a command: {}",
            path.join(" "),
            help.stderr
        );
        for flag in words.iter().filter(|word| word.starts_with("--")) {
            let flag = flag.trim_end_matches([',', '.']);
            assert!(
                help.stdout
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                    .any(|word| word == flag),
                "`toon {}` has no {flag}",
                path.join(" ")
            );
        }
    }
}

#[test]
fn the_skill_keeps_the_glossary_terms() {
    let text = skill_text("operating-an-agent-node");
    assert!(text.contains("a TOON app is a connector with its apps, an app is the service alone"));
    assert!(text.contains("spending_limit"));
    assert!(text.contains("`--yes`"));
}

#[test]
fn skill_install_writes_the_skills_and_is_safe_to_run_again() {
    let machine = Machine::new();
    let file = machine
        .home()
        .join(".claude/skills/operating-an-agent-node/SKILL.md");

    let first = machine.toon(&["skill", "install", "--json"]);
    assert_eq!(first.exit_code, 0, "{}", first.stdout);
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        skill_text("operating-an-agent-node")
    );

    // An older release left something else there.
    fs::write(&file, "an older skill").unwrap();
    let again = machine.toon(&["skill", "install", "--json"]);
    assert_eq!(again.exit_code, 0, "{}", again.stdout);
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        skill_text("operating-an-agent-node")
    );
    assert_eq!(first.json()["skills"], again.json()["skills"]);
}

#[test]
fn skill_install_installs_every_skill_in_the_repository() {
    let machine = Machine::new();
    let run = machine.toon(&["skill", "install", "--json"]);
    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    let report = run.json();
    let installed: Vec<&str> = report["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|skill| skill["name"].as_str().unwrap())
        .collect();
    let shipped = shipped_skills();
    assert_eq!(
        installed,
        shipped
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
    );
    for (name, file) in shipped {
        let source = file.parent().unwrap();
        for relative in skill_files(source) {
            let target = machine
                .home()
                .join(".claude/skills")
                .join(&name)
                .join(&relative);
            assert_eq!(
                fs::read_to_string(&target)
                    .unwrap_or_else(|_| panic!("{} was not installed", target.display())),
                fs::read_to_string(source.join(&relative)).unwrap()
            );
        }
    }
}

#[test]
fn skill_install_takes_a_directory() {
    let machine = Machine::new();
    let target = machine.home().join("elsewhere");
    let run = machine.toon(&["skill", "install", "--dir", target.to_str().unwrap()]);
    assert_eq!(run.exit_code, 0, "{}", run.stderr);
    assert!(target.join("operating-an-agent-node/SKILL.md").exists());
    assert!(!machine.home().join(".claude").exists());
}

#[test]
fn the_nip_skill_walks_through_proposing_and_supporting_a_draft() {
    let text = skill_text("authoring-a-nip");
    for needed in [
        "toon nip new",
        "toon nip publish",
        "toon event query",
        "Is a new NIP warranted",
        "Comment on another agent's draft",
        "Support another agent's draft",
        "draft_refused",
    ] {
        assert!(text.contains(needed), "the skill does not mention {needed}");
    }
}

/// The NIPs of the social set, one reference each: 17 holds 44 and 59 too.
const SOCIAL_NIPS: [&str; 21] = [
    "01", "02", "65", "51", "38", "58", "10", "22", "18", "25", "23", "88", "84", "28", "29", "72",
    "17", "09", "40", "56", "36",
];

fn social_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/social")
}

#[test]
fn the_social_skill_has_one_reference_per_nip_in_the_set() {
    let mut found = skill_files(&social_dir());
    found.retain(|file| file != "SKILL.md");
    let mut wanted: Vec<String> = SOCIAL_NIPS
        .iter()
        .map(|nip| format!("references/nip-{nip}.md"))
        .collect();
    wanted.sort();
    assert_eq!(found, wanted);

    let skill = skill_text("social");
    for nip in SOCIAL_NIPS {
        assert!(
            skill.contains(&format!("references/nip-{nip}.md")),
            "SKILL.md does not list NIP-{nip}"
        );
        let text =
            fs::read_to_string(social_dir().join(format!("references/nip-{nip}.md"))).unwrap();
        assert!(
            text.starts_with(&format!("# NIP-{nip}")),
            "NIP-{nip} has no title"
        );
        for needed in [
            "## Publish",
            "## Read",
            "toon event publish",
            "toon event query",
        ] {
            assert!(text.contains(needed), "NIP-{nip} lacks {needed}");
        }
    }
    let private = fs::read_to_string(social_dir().join("references/nip-17.md")).unwrap();
    assert!(private.contains("NIP-44") && private.contains("NIP-59"));
    for needed in [
        "toon message send",
        "toon message list",
        "--reply-to",
        "--subject",
        "--relay",
        "--yes",
        "--with",
        "--since",
        "--limit",
        "agent_key_not_kept",
        "ADR 0008",
        "does not read a recipient's kind `10050`",
    ] {
        assert!(private.contains(needed), "NIP-17 lacks {needed}");
    }
    assert!(!private.contains("cannot send") && !private.contains("this build cannot"));
}

#[test]
fn no_skill_says_the_relay_lacks_nip_42_or_names_a_private_message_a_dm_or_a_chat() {
    for (name, file) in shipped_skills() {
        let _ = file;
        let text = skill_text(&name);
        assert!(!text.contains("not implemented by the relay"), "{name}");
    }
    let social = skill_text("social");
    assert!(social.contains("the relay implements it"));
    let private = fs::read_to_string(social_dir().join("references/nip-17.md")).unwrap();
    let body = private.split_once('\n').unwrap().1;
    for avoided in ["DM", "direct message", "chat"] {
        assert!(!body.contains(avoided), "NIP-17 says {avoided}");
    }
    let operating = skill_text("operating-an-agent-node");
    assert!(operating.contains("`toon message list`") && operating.contains("`toon message send"));
}

#[test]
fn the_social_skill_states_cost_gaps_and_omissions() {
    let text = skill_text("social");
    for needed in [
        "a TOON app is a connector with its apps, an app is the service alone",
        "toon limit show",
        "not_confirmed",
        "toon relay subscriptions",
        "NIP-29",
        "NIP-42",
        "NIP-57",
        "NIP-05",
    ] {
        assert!(text.contains(needed), "the skill does not mention {needed}");
    }
    let omitted = text
        .split("## Left out, and why")
        .nth(1)
        .expect("a section on what is left out");
    assert!(omitted.contains("Lightning") && omitted.contains("clearnet domain"));
}

/// `npx skills add` reads a `SKILL.md`'s frontmatter as YAML and skips a skill it cannot
/// parse. A plain description holding `: ` is not YAML, so each description is quoted.
#[test]
fn every_skill_has_frontmatter_a_skills_installer_can_read() {
    for (name, file) in shipped_skills() {
        let text = fs::read_to_string(&file).expect("read a skill");
        let frontmatter = text
            .strip_prefix("---\n")
            .and_then(|rest| rest.split_once("\n---\n"))
            .map(|(frontmatter, _)| frontmatter)
            .unwrap_or_else(|| panic!("{name}: SKILL.md does not start with frontmatter"));
        let field = |key: &str| {
            frontmatter
                .lines()
                .find_map(|line| line.strip_prefix(key)?.strip_prefix(": "))
                .unwrap_or_else(|| panic!("{name}: the frontmatter has no {key}"))
        };
        assert_eq!(field("name"), name, "the skill's name is its directory's");
        let description = field("description");
        let quoted = description
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or_else(|| panic!("{name}: the description is not a quoted string"));
        assert!(
            !quoted.is_empty() && !quoted.contains(['"', '\\']),
            "{name}: the description holds a character the quotes would need escaped"
        );
    }
}
