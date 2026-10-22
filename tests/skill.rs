mod support;

use std::fs;
use std::path::{Path, PathBuf};

use support::Machine;

/// The vendored `toon-plugins` plugins, each a directory with its skills under `skills/`.
fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/toon-plugins/plugins")
}

/// The skills of the vendored `toon-nostr` and `toon-cli` plugins: each directory's name and its
/// `SKILL.md`.
fn shipped_skills() -> Vec<(String, PathBuf)> {
    let mut skills: Vec<(String, PathBuf)> = ["toon-nostr", "toon-cli"]
        .iter()
        .flat_map(|plugin| {
            fs::read_dir(plugins_dir().join(plugin).join("skills"))
                .expect("read a plugin's skills/")
                .map(|entry| entry.expect("read skills/").path())
                .filter(|path| path.is_dir())
                .collect::<Vec<_>>()
        })
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, path.join("SKILL.md"))
        })
        .collect();
    skills.sort();
    skills
}

/// The `SKILL.md` of the vendored skill `name`.
fn skill_text(name: &str) -> String {
    let (_, file) = shipped_skills()
        .into_iter()
        .find(|(skill, _)| skill == name)
        .unwrap_or_else(|| panic!("no vendored skill {name}"));
    fs::read_to_string(file).expect("read the skill")
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
    assert!(
        named_commands(&skill_text("sending-with-toon-cli")).len() > 20,
        "the sending skill names its commands"
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
            > 5,
        "the sending skills show their event commands"
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

/// The range of `toon_cli` versions the vendored sending skill supports, as comparators:
/// `>=0.3, <0.4` is two.
fn supported_range() -> Vec<(String, Vec<u64>)> {
    let text = skill_text("sending-with-toon-cli");
    let range = text
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .and_then(|(frontmatter, _)| {
            frontmatter
                .lines()
                .find_map(|line| line.strip_prefix("toon_cli: "))
        })
        .expect("the sending skill's frontmatter has a toon_cli range")
        .trim()
        .trim_matches('"');
    range
        .split(',')
        .map(|comparator| {
            let comparator = comparator.trim();
            let split = comparator
                .find(|c: char| c.is_ascii_digit())
                .unwrap_or_else(|| panic!("no version in `{comparator}`"));
            let (operator, version) = comparator.split_at(split);
            let parts = version
                .split('.')
                .map(|part| part.parse().expect("a numeric version part"))
                .collect();
            (operator.trim().to_owned(), parts)
        })
        .collect()
}

/// Whether `version` satisfies one comparator; a missing part counts as zero.
fn satisfies(version: &[u64], operator: &str, bound: &[u64]) -> bool {
    let part = |parts: &[u64], index: usize| parts.get(index).copied().unwrap_or(0);
    let ordering = (0..3)
        .map(|index| part(version, index).cmp(&part(bound, index)))
        .find(|ordering| ordering.is_ne())
        .unwrap_or(std::cmp::Ordering::Equal);
    match operator {
        ">=" => ordering.is_ge(),
        ">" => ordering.is_gt(),
        "<=" => ordering.is_le(),
        "<" => ordering.is_lt(),
        "=" | "" => ordering.is_eq(),
        other => panic!("the range uses an operator this test does not read: {other}"),
    }
}

#[test]
fn the_sending_skill_supports_this_crates_version() {
    let version: Vec<u64> = env!("CARGO_PKG_VERSION")
        .split(['.', '-'])
        .take(3)
        .map(|part| part.parse().expect("a numeric crate version"))
        .collect();
    for (operator, bound) in supported_range() {
        assert!(
            satisfies(&version, &operator, &bound),
            "the vendored sending-with-toon-cli skill supports toon_cli {operator}{bound:?}, \
             not {}: refresh vendor/toon-plugins",
            env!("CARGO_PKG_VERSION")
        );
    }
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
fn skill_install_ships_both_plugins_and_never_the_client_plugin() {
    let machine = Machine::new();
    let target = machine.home().join("flat");
    let run = machine.toon(&["skill", "install", "--dir", target.to_str().unwrap()]);
    assert_eq!(run.exit_code, 0, "{}", run.stderr);
    let mut installed: Vec<String> = fs::read_dir(&target)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    installed.sort();
    assert_eq!(
        installed,
        [
            "authoring-a-nip",
            "operating-an-agent-node",
            "sending-with-toon-cli",
            "social"
        ]
    );
    assert!(target.join("social/references/nip-01.md").exists());
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

#[test]
fn skill_list_names_every_skill_and_reference_that_install_writes() {
    let machine = Machine::new();
    let run = machine.toon(&["skill", "list", "--json"]);
    assert_eq!(run.exit_code, 0, "{}", run.stderr);
    let listed = run.json();
    let shipped = shipped_skills();
    let skills = listed["skills"].as_array().unwrap();
    assert_eq!(skills.len(), shipped.len());
    for (skill, (name, file)) in skills.iter().zip(&shipped) {
        assert_eq!(skill["name"], name.as_str());
        let mut references: Vec<&str> = skill["references"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();
        references.sort();
        let mut wanted = skill_files(file.parent().unwrap());
        wanted.retain(|f| f != "SKILL.md");
        assert_eq!(references, wanted);
    }
    let text = machine.toon(&["skill", "list"]);
    assert!(
        text.stdout.contains("social\n  references/nip-01.md")
            && text.stdout.contains("  references/nip-02.md")
    );
    let rev =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/toon-plugins/REV"))
            .unwrap();
    assert_eq!(listed["toon_plugins_rev"], rev.trim());
    assert!(text.stdout.contains(rev.trim()), "{}", text.stdout);
}

#[test]
fn skill_show_prints_the_bytes_install_writes() {
    let machine = Machine::new();
    for (name, file) in shipped_skills() {
        let dir = file.parent().unwrap();
        for path in skill_files(dir) {
            let mut args = vec!["skill", "show", name.as_str()];
            if path != "SKILL.md" {
                args.push(path.as_str());
            }
            let shown = machine.toon(&args);
            assert_eq!(shown.exit_code, 0, "{name} {path}: {}", shown.stderr);
            let wanted = fs::read_to_string(dir.join(&path)).unwrap();
            assert_eq!(shown.stdout, wanted, "{name} {path}");
            args.push("--json");
            assert_eq!(machine.toon(&args).json()["text"], wanted.as_str());
        }
    }
}

#[test]
fn skill_show_refuses_what_does_not_exist_and_says_what_does() {
    let machine = Machine::new();
    let skill = machine.toon(&["skill", "show", "nope"]);
    assert_eq!(skill.exit_code, 2);
    assert!(skill.stderr.contains("social"), "{}", skill.stderr);
    let json = machine.toon(&["skill", "show", "nope", "--json"]);
    assert_eq!(json.exit_code, 2);
    assert_eq!(json.json()["error"]["code"], "usage");
    let reference = machine.toon(&["skill", "show", "social", "references/nope.md", "--json"]);
    assert_eq!(reference.exit_code, 2);
    assert_eq!(reference.json()["error"]["code"], "usage");
    assert!(reference.stdout.contains("references/nip-02.md"));
}

#[test]
fn skill_list_and_show_are_in_help() {
    let machine = Machine::new();
    let help = machine.toon(&["--help"]);
    assert!(
        help.stdout
            .contains("Install, list or show the skills shipped in this binary"),
        "{}",
        help.stdout
    );
    let help = machine.toon(&["skill", "--help"]);
    assert!(help.stdout.contains("list") && help.stdout.contains("show"));
}
