mod support;

use std::fs;
use std::path::Path;

use support::Machine;

/// The exit codes, as fixed by the releases so far. A code is added here and never
/// renumbered or given a new meaning.
const EXIT_CODES: [(u8, &str); 4] = [
    (0, "The command did what was asked"),
    (1, "The command failed; the error's code says why"),
    (2, "The command line was not understood"),
    (3, "There is no agent node on this machine"),
];

#[test]
fn help_lists_the_exit_codes() {
    let machine = Machine::new();

    let run = machine.toon(&["--help"]);

    let listed: Vec<&str> = run
        .stdout
        .lines()
        .skip_while(|line| *line != "Exit codes:")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .collect();
    let expected: Vec<String> = EXIT_CODES
        .iter()
        .map(|(code, meaning)| format!("  {code}  {meaning}"))
        .collect();
    assert_eq!(listed, expected);
    assert_eq!(run.exit_code, 0);
}

fn exit_codes_document() -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/exit-codes.md"))
        .expect("read docs/exit-codes.md")
}

#[test]
fn the_exit_codes_document_lists_the_same_codes() {
    let document = exit_codes_document();

    let rows: Vec<&str> = document
        .lines()
        .filter(|line| {
            line.strip_prefix("| ")
                .is_some_and(|row| row.starts_with(|first: char| first.is_ascii_digit()))
        })
        .collect();
    let expected: Vec<String> = EXIT_CODES
        .iter()
        .map(|(code, meaning)| format!("| {code} | {meaning} |"))
        .collect();
    assert_eq!(rows, expected);
}

/// The error codes, as fixed by the releases so far: `tests/error_codes.txt`, a line to a
/// code. A new one is added there and to the document together.
const ERROR_CODES: &str = include_str!("error_codes.txt");

#[test]
fn the_exit_codes_document_lists_the_error_codes() {
    let document = exit_codes_document();

    for code in ERROR_CODES.lines().filter(|line| !line.is_empty()) {
        assert!(
            document.contains(&format!("| `{code}` |")),
            "docs/exit-codes.md does not list the error code `{code}`"
        );
    }
}
