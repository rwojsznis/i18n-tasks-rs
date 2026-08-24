use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_i18n-tasks-rs");

struct Project {
    root: PathBuf,
    config: PathBuf,
}

const BLOCK_CONFIG: &str = "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\nignore_missing:\n  # Keep this explanation.\n  - missing.live\n  # This comment belongs to the stale rule.\n  - missing.gone\nignore_unused:\n  - unused\n  - used\nignore:\n  - absent.everywhere\n";

impl Project {
    fn new(name: &str) -> Project {
        Project::with_config(name, BLOCK_CONFIG)
    }

    fn with_config(name: &str, config_body: &str) -> Project {
        let root = std::env::temp_dir().join(format!("i18n-tasks-rs-clean-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config/locales")).unwrap();
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(
            root.join("config/locales/en.yml"),
            "---\nen:\n  used: Used\n  unused: Unused\n",
        )
        .unwrap();
        std::fs::write(root.join("app/use.rb"), "t('used')\nt('missing.live')\n").unwrap();
        let config = root.join("config/i18n-tasks-rs.yml");
        std::fs::write(&config, config_body).unwrap();
        Project { root, config }
    }

    fn run(&self, extra: &[&str]) -> (i32, String) {
        let mut command = Command::new(BIN);
        command
            .arg("clean-config")
            .arg("--config")
            .arg(&self.config)
            .arg("--root")
            .arg(&self.root)
            .args(extra);
        let out = command.output().unwrap();
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn reports_stale_rules_without_writing() {
    let project = Project::new("report");
    let before = std::fs::read_to_string(&project.config).unwrap();

    let (code, output) = project.run(&[]);

    assert_eq!(code, 1, "{output}");
    assert!(output.contains("-  - missing.gone"), "{output}");
    assert!(output.contains("-  - used"), "{output}");
    assert!(output.contains("-  - absent.everywhere"), "{output}");
    assert!(output.contains("Nothing was written"), "{output}");
    assert_eq!(std::fs::read_to_string(&project.config).unwrap(), before);
}

#[test]
fn write_removes_only_stale_rules_and_their_comments() {
    let project = Project::new("write");

    let (code, output) = project.run(&["--write"]);

    assert_eq!(code, 0, "{output}");
    let cleaned = std::fs::read_to_string(&project.config).unwrap();
    assert!(cleaned.contains("# Keep this explanation.\n  - missing.live"));
    assert!(!cleaned.contains("missing.gone"));
    assert!(!cleaned.contains("comment belongs to the stale rule"));
    assert!(cleaned.contains("ignore_unused:\n  - unused"));
    assert!(!cleaned.contains("  - used\n"));
    assert!(!cleaned.contains("absent.everywhere"));

    let (second_code, second_output) = project.run(&[]);
    assert_eq!(second_code, 0, "{second_output}");
    assert!(second_output.contains("Config ignore rules are clean"));
}

/// A flow-style list holds several rules on one line, so a line-wise removal
/// takes the live neighbours of a stale rule with it. The live rule must
/// survive `--write`; the stale one is reported for a human to remove.
#[test]
fn a_flow_style_list_keeps_its_live_rules() {
    let project = Project::with_config(
        "flow",
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\nignore_unused: [\"unused\", \"zzz.*\"]\n",
    );
    let before = std::fs::read_to_string(&project.config).unwrap();

    let (code, output) = project.run(&["--write"]);

    assert_eq!(code, 1, "{output}");
    assert_eq!(std::fs::read_to_string(&project.config).unwrap(), before);
    assert!(output.contains("zzz.*"), "{output}");
    assert!(output.contains("flow style"), "{output}");

    // Nothing was written, so the JSON must not claim it was.
    let (json_code, json_output) = project.run(&["-f", "json", "--write"]);
    assert_eq!(json_code, 1, "{json_output}");
    let json: serde_json::Value = serde_json::from_str(&json_output).unwrap();
    assert_eq!(json["written"], false, "{json_output}");
}

/// When every rule on a shared line is stale, the line goes as a whole.
#[test]
fn a_flow_style_list_of_only_stale_rules_is_removed() {
    let project = Project::with_config(
        "flow-all-stale",
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\nignore_unused: [\"yyy.*\", \"zzz.*\"]\n",
    );

    let (code, output) = project.run(&["--write"]);

    assert_eq!(code, 0, "{output}");
    let cleaned = std::fs::read_to_string(&project.config).unwrap();
    assert!(!cleaned.contains("ignore_unused"), "{cleaned}");
}

/// The JSON report says which stale rules the write cannot remove.
#[test]
fn json_marks_the_rules_a_human_has_to_remove() {
    let project = Project::with_config(
        "flow-json",
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\nignore_unused: [\"unused\", \"zzz.*\"]\nignore:\n  - absent.everywhere\n",
    );

    let (code, output) = project.run(&["-f", "json", "--write"]);

    assert_eq!(code, 1, "{output}");
    let json: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(json["written"], true, "{output}");
    let rules = json["stale_rules"].as_array().unwrap();
    let manual: Vec<&serde_json::Value> = rules
        .iter()
        .filter(|r| r["manual"].as_bool().unwrap())
        .collect();
    assert_eq!(manual.len(), 1, "{output}");
    assert_eq!(manual[0]["pattern"], "zzz.*");
    let automatic: Vec<&serde_json::Value> = rules
        .iter()
        .filter(|r| !r["manual"].as_bool().unwrap())
        .collect();
    assert_eq!(automatic.len(), 1, "{output}");
    assert_eq!(automatic[0]["pattern"], "absent.everywhere");
}
