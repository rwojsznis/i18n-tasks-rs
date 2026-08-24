//! A reader that quits early (`| head`, `| more` then `q`) closes the pipe.
//!
//! Rust ignores `SIGPIPE`, so the failed write comes back as an error and
//! `println!` panics on it. That is not a tool failure: the output simply has
//! nowhere left to go.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_i18n-tasks-rs");

struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let root = std::env::temp_dir().join(format!("i18n-tasks-rs-pipe-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config/locales")).unwrap();
        std::fs::create_dir_all(root.join("app/controllers")).unwrap();
        // Enough unused keys that the report is long, as a real one is.
        let mut yml = String::from("en:\n");
        for i in 0..2000 {
            yml.push_str(&format!("  key_{i}: value {i}\n"));
        }
        std::fs::write(root.join("config/locales/en.yml"), yml).unwrap();
        std::fs::write(
            root.join("config/i18n-tasks.yml"),
            "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
        )
        .unwrap();
        std::fs::write(root.join("app/controllers/a_controller.rb"), "t('key_0')\n").unwrap();
        Project { root }
    }

    /// Runs the command, closes the read end of its stdout at once, and
    /// returns the exit code and whatever it said on stderr.
    fn run_and_hang_up(&self, args: &[&str]) -> (i32, String) {
        let mut child = Command::new(BIN)
            .args(args)
            .arg("-c")
            .arg(self.root.join("config/i18n-tasks.yml"))
            .arg("--root")
            .arg(&self.root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("binary runs");
        drop(child.stdout.take());
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("stderr is piped")
            .read_to_string(&mut stderr)
            .expect("stderr reads");
        let status = child.wait().expect("child exits");
        (status.code().unwrap_or(-1), stderr)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_closed_stdout_is_not_a_panic() {
    let p = Project::new("closed");
    for args in [
        vec!["unused"],
        vec!["missing"],
        vec!["find"],
        vec!["health"],
        vec!["normalize"],
        vec!["unused", "-f", "json"],
    ] {
        let (code, stderr) = p.run_and_hang_up(&args);
        assert!(
            !stderr.contains("panicked"),
            "`{args:?}` panicked on a closed pipe:\n{stderr}"
        );
        assert_ne!(code, 101, "`{args:?}` exited with the panic code");
    }
}
