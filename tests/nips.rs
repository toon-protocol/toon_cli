use std::fs;
use std::path::{Path, PathBuf};

fn nips_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("nips")
}

fn file_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .expect("a file in nips/ has a name in UTF-8")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// The `## ` headings of a draft, in order. A draft may divide a section further.
fn sections(document: &str) -> Vec<&str> {
    document
        .lines()
        .filter_map(|line| line.strip_prefix("## "))
        .collect()
}

/// Every draft in `nips/`: each Markdown file but the template and the README.
fn drafts() -> Vec<PathBuf> {
    let mut drafts: Vec<PathBuf> = fs::read_dir(nips_dir())
        .expect("read nips/")
        .map(|entry| entry.expect("read an entry of nips/").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .filter(|path| !["TEMPLATE.md", "README.md"].contains(&file_name(path)))
        .collect();
    drafts.sort();
    drafts
}

#[test]
fn every_draft_has_the_sections_of_the_template() {
    let template = read(&nips_dir().join("TEMPLATE.md"));
    let expected = sections(&template);
    assert!(!expected.is_empty(), "nips/TEMPLATE.md has no sections");

    let drafts = drafts();
    assert!(!drafts.is_empty(), "nips/ holds no draft");
    for draft in drafts {
        assert_eq!(
            sections(&read(&draft)),
            expected,
            "{} does not have the template's sections, in the template's order",
            draft.display()
        );
    }
}

#[test]
fn every_draft_says_it_is_a_draft() {
    for draft in drafts() {
        let document = read(&draft);
        let mut lines = document.lines().filter(|line| !line.is_empty());
        let title = lines.next().unwrap_or("");
        let status = lines.next().unwrap_or("");
        assert!(
            title.starts_with("# "),
            "{} does not start with its title",
            draft.display()
        );
        assert!(
            status.split_whitespace().any(|word| word == "`draft`"),
            "{} does not say `draft` under its title",
            draft.display()
        );
    }
}

#[test]
fn the_readme_lists_every_draft() {
    let readme = read(&nips_dir().join("README.md"));

    for draft in drafts() {
        let name = file_name(&draft);
        assert!(
            readme.contains(&format!("]({name})")),
            "nips/README.md does not link to {name}"
        );
    }
}
