mod support;

use std::fs;
use std::path::Path;

use support::Machine;

fn skill_text() -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/operating-an-agent-node/SKILL.md"),
    )
    .expect("read the skill")
}

/// Every inline code span of the skill that starts with `toon `, as its words.
fn named_commands(text: &str) -> Vec<Vec<String>> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter_map(|span| span.strip_prefix("toon "))
        .map(|rest| rest.split_whitespace().map(str::to_owned).collect())
        .collect()
}

#[test]
fn every_command_the_skill_names_exists_in_the_binary() {
    let machine = Machine::new();
    let commands = named_commands(&skill_text());
    assert!(
        commands.len() > 30,
        "the skill names its commands: {commands:?}"
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
                help.stdout.contains(flag),
                "`toon {}` has no {flag}",
                path.join(" ")
            );
        }
    }
}

#[test]
fn the_skill_keeps_the_glossary_terms() {
    let text = skill_text();
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
    assert_eq!(fs::read_to_string(&file).unwrap(), skill_text());

    // An older release left something else there.
    fs::write(&file, "an older skill").unwrap();
    let again = machine.toon(&["skill", "install", "--json"]);
    assert_eq!(again.exit_code, 0, "{}", again.stdout);
    assert_eq!(fs::read_to_string(&file).unwrap(), skill_text());
    assert_eq!(first.json()["skills"], again.json()["skills"]);
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
