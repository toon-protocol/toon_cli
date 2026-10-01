//! `toon nip new` and `toon nip publish`: an agent proposes protocol as an event.
//!
//! The draft is the single source (`nips/proposals-as-events.md`, "From a file to an
//! event"): a publisher derives the kind `30817` event from the file, so the same file
//! always gives the same draft, and signs it with the agent identity (ADR 0004).

use std::path::Path;

use serde_json::{json, Value};

use crate::cli::NipCommand;
use crate::event;
use crate::home;
use crate::node;
use crate::outcome::{Error, ErrorCode, Exit, Report};

/// The kind of a draft: NostrHub's "custom NIP".
const DRAFT_KIND: u64 = 30817;

/// The template `nip new` copies.
const TEMPLATE: &str = include_str!("../nips/TEMPLATE.md");

fn usage(message: impl Into<String>) -> Error {
    Error {
        code: ErrorCode::Usage,
        message: message.into(),
    }
}

/// A draft the proposals draft says not to write or publish.
fn refused(message: impl Into<String>) -> Error {
    Error {
        code: ErrorCode::DraftRefused,
        message: message.into(),
    }
}

/// A valid identifier: 1 to 64 characters, each a lower-case letter, a digit or a hyphen.
fn valid_identifier(identifier: &str) -> bool {
    (1..=64).contains(&identifier.len())
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The file name a title is given: lower-case words joined by hyphens.
fn slug(title: &str) -> String {
    let mut slug = String::new();
    for character in title.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    slug.truncate(64);
    slug.trim_end_matches('-').to_owned()
}

/// `toon nip`.
pub fn run(command: NipCommand) -> Result<Report, Error> {
    match command {
        NipCommand::New { title } => {
            let directory = std::env::current_dir().map_err(|error| Error {
                code: ErrorCode::Io,
                message: format!("The current directory is unreadable: {error}."),
            })?;
            new(&directory, &title)
        }
        NipCommand::Publish {
            draft,
            relay,
            topics,
            title_changed,
            amount,
        } => publish(
            &home::resolve()?,
            &draft,
            &relay,
            &topics,
            title_changed,
            amount,
        ),
    }
}

/// `toon nip new`: write a draft called `title` from the template into `directory`.
pub fn new(directory: &Path, title: &str) -> Result<Report, Error> {
    let title = title.trim();
    if title.is_empty() || title.contains('\n') {
        return Err(usage("The title must be one non-empty line."));
    }
    let identifier = slug(title);
    if identifier.is_empty() {
        return Err(usage(
            "The title must have a letter or a digit to name the file after.",
        ));
    }
    let path = directory.join(format!("{identifier}.md"));
    if path.exists() {
        return Err(refused(format!(
            "{} exists already; `nip new` does not overwrite a draft.",
            path.display()
        )));
    }
    let body = TEMPLATE
        .strip_prefix("# Title\n")
        .expect("the template begins with its title");
    node::write(&path, format!("# {title}\n{body}").as_bytes(), 0o644)?;
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "outcome": "created", "path": path, "identifier": identifier }),
        text: format!("Wrote {}.", path.display()),
    })
}

/// What a draft's file says about its event.
#[derive(Debug, PartialEq)]
struct Draft {
    identifier: String,
    title: String,
    summary: Option<String>,
    /// One `(kind, name)` for each row of the kinds table that the draft defines.
    kinds: Vec<(String, String)>,
}

fn cells(row: &str) -> Vec<&str> {
    row.trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

fn parse(file_name: &str, document: &str) -> Result<Draft, Error> {
    let identifier = file_name.strip_suffix(".md").unwrap_or(file_name);
    if !valid_identifier(identifier) {
        return Err(refused(format!(
            "{file_name} does not name a draft: the file's name without .md must be 1 to 64 \
             lower-case letters, digits or hyphens."
        )));
    }
    let title = document
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("# "))
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .ok_or_else(|| {
            refused(format!(
                "{file_name} does not begin with a `# ` title line."
            ))
        })?;

    // The first paragraph after the line that says `draft`. A comment is not a paragraph.
    let lines: Vec<&str> = document.lines().collect();
    let summary = lines
        .iter()
        .position(|line| line.split_whitespace().any(|word| word == "`draft`"))
        .and_then(|status| {
            let paragraph: Vec<&str> = lines[status + 1..]
                .iter()
                .skip_while(|line| line.trim().is_empty())
                .take_while(|line| !line.trim().is_empty())
                .copied()
                .collect();
            let text = paragraph.join(" ");
            (!text.is_empty() && !text.starts_with("<!--")).then_some(text)
        });

    let mut kinds = Vec::new();
    let mut in_kinds = false;
    for line in &lines {
        if let Some(heading) = line.strip_prefix("## ") {
            in_kinds = heading.trim() == "Kinds";
        } else if in_kinds && line.trim_start().starts_with('|') {
            let row = cells(line);
            if row.len() >= 2 && row.last() == Some(&"This draft") {
                if let Some(number) = row[0].strip_prefix('`').and_then(|n| n.strip_suffix('`')) {
                    kinds.push((number.to_owned(), row[1].to_owned()));
                }
            }
        }
    }
    Ok(Draft {
        identifier: identifier.to_owned(),
        title: title.to_owned(),
        summary,
        kinds,
    })
}

/// The tags of the draft's event, in the order the draft lists them.
fn tags(draft: &Draft, topics: &[String]) -> Value {
    let mut tags = vec![
        json!(["d", draft.identifier]),
        json!(["title", draft.title]),
    ];
    tags.extend(
        draft
            .kinds
            .iter()
            .map(|(kind, name)| json!(["k", kind, name])),
    );
    if let Some(summary) = &draft.summary {
        tags.push(json!(["summary", summary]));
    }
    tags.extend(topics.iter().map(|topic| json!(["t", topic])));
    Value::Array(tags)
}

fn tag<'a>(event: &'a Value, name: &str) -> Option<&'a str> {
    event["tags"]
        .as_array()?
        .iter()
        .find(|tag| tag[0] == name)
        .and_then(|tag| tag[1].as_str())
}

/// The current revision: the greatest `created_at`, and of two the same, the lower id.
fn current_revision<'a>(events: &'a [Value], author: &str, identifier: &str) -> Option<&'a Value> {
    events
        .iter()
        .filter(|event| {
            event["kind"] == DRAFT_KIND
                && event["pubkey"] == author
                && tag(event, "d") == Some(identifier)
        })
        .min_by(|a, b| {
            let created = |event: &Value| event["created_at"].as_u64().unwrap_or(0);
            let id = |event: &'a Value| event["id"].as_str().unwrap_or_default();
            created(b).cmp(&created(a)).then_with(|| id(a).cmp(id(b)))
        })
}

/// `toon nip publish`: sign the draft in `path` with the agent identity and write it to
/// the agent node's own relay. `relay` is asked first for the current revision of the
/// draft, which is where `created_at` and the title check come from.
pub fn publish(
    home: &Path,
    path: &Path,
    relay: &str,
    topics: &[String],
    title_changed: bool,
    amount: u64,
) -> Result<Report, Error> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| usage(format!("{} is not the path of a file.", path.display())))?;
    let document = std::fs::read(path).map_err(|error| Error {
        code: ErrorCode::Io,
        message: format!("{} could not be read: {error}.", path.display()),
    })?;
    let document = String::from_utf8(document)
        .map_err(|_| refused(format!("{} is not UTF-8 text.", path.display())))?;
    let draft = parse(file_name, &document)?;
    if let Some(topic) = topics
        .iter()
        .find(|topic| topic.is_empty() || topic.to_lowercase() != **topic)
    {
        return Err(usage(format!(
            "{topic:?} is not a topic: one in lower case."
        )));
    }

    if node::State::load(home)?.is_none() {
        return Err(node::no_agent_node(home));
    }
    let secret = event::agent_secret(home)?;
    let author = event::public_key(&secret)?;

    let found = event::fetch(
        relay,
        &json!({ "kinds": [DRAFT_KIND], "authors": [author], "#d": [draft.identifier] }),
    )?;
    let mut created_at = event::now();
    if let Some(revision) = current_revision(&found, &author, &draft.identifier) {
        let earlier = tag(revision, "title").unwrap_or_default();
        if earlier != draft.title && !title_changed {
            return Err(refused(format!(
                "{relay} holds a draft {} by this identity under another title, {earlier:?}. \
                 That is a sign the identifier belongs to another draft; rename the file, or \
                 pass --title-changed if the title has changed.",
                draft.identifier
            )));
        }
        created_at = created_at.max(revision["created_at"].as_u64().unwrap_or(0) + 1);
    }
    let signed = event::sign(
        &secret,
        created_at,
        DRAFT_KIND,
        tags(&draft, topics),
        &document,
    )?;
    event::write(home, signed, amount)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template_draft() -> Draft {
        parse("example.md", TEMPLATE).unwrap()
    }

    #[test]
    fn a_title_is_named_after_its_words() {
        assert_eq!(slug("Paid  Subscription: v2!"), "paid-subscription-v2");
        assert_eq!(slug("!!!"), "");
    }

    #[test]
    fn the_template_is_a_draft_with_no_kind_and_no_summary() {
        let draft = template_draft();
        assert_eq!(draft.title, "Title");
        assert!(draft.kinds.is_empty());
        assert_eq!(draft.summary, None);
    }

    #[test]
    fn a_draft_gives_its_summary_and_the_kinds_it_defines() {
        let document =
            "# Pings\n\n`draft` `optional`\n\nTwo parties\ncan ping.\n\nMore.\n\n## Kinds\n\n\
            | Kind | Event | Class | Signed with | Defined by |\n| --- | --- | --- | --- | --- |\n\
            | `9000` | Ping | regular | Anyone | This draft |\n\
            | `1` | Note | regular | Anyone | NIP-01 |\n";
        let draft = parse("pings.md", document).unwrap();
        assert_eq!(draft.summary.as_deref(), Some("Two parties can ping."));
        assert_eq!(draft.kinds, vec![("9000".to_owned(), "Ping".to_owned())]);
        assert_eq!(
            tags(&draft, &["agents".to_owned()]),
            json!([
                ["d", "pings"],
                ["title", "Pings"],
                ["k", "9000", "Ping"],
                ["summary", "Two parties can ping."],
                ["t", "agents"]
            ])
        );
    }

    #[test]
    fn a_file_that_is_not_a_draft_is_refused() {
        assert!(parse("Bad_Name.md", "# T\n").is_err());
        assert!(parse("ok.md", "no title\n").is_err());
        assert!(parse("ok.md", "#  \n").is_err());
        assert!(parse(&format!("{}.md", "a".repeat(65)), "# T\n").is_err());
    }

    #[test]
    fn the_current_revision_is_the_newest_and_of_a_tie_the_lowest_id() {
        let revision = |id: &str, created_at: u64| {
            json!({ "id": id, "pubkey": "a", "kind": DRAFT_KIND, "created_at": created_at,
                    "tags": [["d", "x"]] })
        };
        let events = [revision("b", 5), revision("c", 7), revision("a", 7)];
        let current = current_revision(&events, "a", "x").unwrap();
        assert_eq!(current["id"], "a");
        assert!(current_revision(&events, "z", "x").is_none());
    }
}
