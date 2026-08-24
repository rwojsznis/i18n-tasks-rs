use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_i18n-tasks-rs");

struct Project {
    root: PathBuf,
    config: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
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
        std::fs::write(
            &config,
            "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\nignore_missing:\n  # Keep this explanation.\n  - missing.live\n  # This comment belongs to the stale rule.\n  - missing.gone\nignore_unused:\n  - unused\n  - used\nignore:\n  - absent.everywhere\n",
        )
        .unwrap();
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
