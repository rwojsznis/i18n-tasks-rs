//! `missing -f json` against the gem's own output, byte for byte.
//!
//! ref: lib/i18n/tasks/command/commands/missing.rb:30-38
//! ref: lib/i18n/tasks/command/options/data.rb#print_forest
//!
//! The goldens are what i18n-tasks 1.1.2 printed for `tests/fixtures/missing_json`,
//! with the arguments named in each test, run from the fixture directory so it
//! reads `config/i18n-tasks.yml` there.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_i18n-tasks-rs");
const FIXTURE: &str = "tests/fixtures/missing_json";

fn missing(args: &[&str]) -> (i32, String) {
    let out = Command::new(BIN)
        .arg("missing")
        .args(args)
        .arg("-c")
        .arg(format!("{FIXTURE}/config/i18n-tasks.yml"))
        .arg("--root")
        .arg(FIXTURE)
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
    )
}

fn golden(name: &str) -> String {
    std::fs::read_to_string(format!("tests/golden/{name}.json")).unwrap()
}

#[test]
fn diff_equals_the_gems() {
    assert_eq!(
        missing(&["-t", "diff", "-f", "json"]),
        (1, golden("missing_diff"))
    );
}

#[test]
fn diff_for_one_locale_equals_the_gems() {
    assert_eq!(
        missing(&["-t", "diff", "-f", "json", "de"]),
        (1, golden("missing_diff_de"))
    );
}

#[test]
fn used_equals_the_gems() {
    assert_eq!(
        missing(&["-t", "used", "-f", "json"]),
        (1, golden("missing_used"))
    );
}

#[test]
fn plural_equals_the_gems() {
    assert_eq!(
        missing(&["-t", "plural", "-f", "json"]),
        (1, golden("missing_plural"))
    );
}

#[test]
fn every_type_merged_equals_the_gems() {
    assert_eq!(missing(&["-f", "json"]), (1, golden("missing_all")));
}

#[test]
fn nothing_missing_is_an_empty_object() {
    assert_eq!(
        missing(&["-t", "plural", "-f", "json", "en"]),
        (0, "{}\n".to_string())
    );
}
