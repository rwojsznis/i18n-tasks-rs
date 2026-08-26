//! The parallel scan must not change a single byte.
//!
//! `rayon` fans the source scan out over the file list, so the merge order is
//! no longer the file order. Every sort that survives into the output has to be
//! total, and every per-file result has to be merged in file order. A parallel
//! run that reorders output is a bug.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_i18n-tasks-rs");

/// A project big enough that the pool really does hand work to several threads,
/// and varied enough that every scanner contributes.
struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let root = std::env::temp_dir().join(format!("i18n-tasks-rs-jobs-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config/locales")).unwrap();
        std::fs::create_dir_all(root.join("app/views/posts")).unwrap();
        std::fs::create_dir_all(root.join("app/controllers")).unwrap();
        std::fs::create_dir_all(root.join("app/javascript")).unwrap();

        let mut locale = String::from("---\nen:\n");
        for i in 0..120 {
            // Two calls on one line share a path and a position prefix, which is
            // the case a non-total sort would reorder.
            std::fs::write(
                root.join(format!("app/controllers/c{i}_controller.rb")),
                format!(
                    "class C{i}Controller\n  def show\n    t('.title'); t('shared.k{i}')\n  end\nend\n"
                ),
            )
            .unwrap();
            std::fs::write(
                root.join(format!("app/views/posts/_p{i}.html.erb")),
                format!("<%= t('erb.k{i}') %><%= t('shared.k{i}') %>\n"),
            )
            .unwrap();
            std::fs::write(
                root.join(format!("app/views/posts/_s{i}.html.slim")),
                format!("= t('slim.k{i}')\n= t('shared.k{i}')\n"),
            )
            .unwrap();
            std::fs::write(
                root.join(format!("app/javascript/m{i}.js")),
                format!("I18n.t('js.k{i}');\n"),
            )
            .unwrap();
            // An interpolated key, so key patterns are exercised too (B5).
            std::fs::write(
                root.join(format!("app/controllers/d{i}_controller.rb")),
                format!("t(\"dyn.k{i}.#{{kind}}\")\nt(some_variable)\n"),
            )
            .unwrap();

            locale.push_str(&format!(
                "  shared:\n    k{i}: S{i}\n  erb:\n    k{i}: E{i}\n  slim:\n    k{i}: L{i}\n  js:\n    k{i}: J{i}\n  unused{i}: U{i}\n"
            ));
            locale.push_str(&format!("  c{i}:\n    show:\n      title: T{i}\n"));
        }
        std::fs::write(root.join("config/locales/en.yml"), locale).unwrap();
        std::fs::write(
            root.join("config/i18n-tasks.yml"),
            "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n  relative_roots: [app/views, app/controllers]\n",
        )
        .unwrap();
        Project { root }
    }

    fn run(&self, args: &[&str]) -> (i32, Vec<u8>) {
        let out = Command::new(BIN)
            .args(args)
            .arg("-c")
            .arg(self.root.join("config/i18n-tasks.yml"))
            .arg("--root")
            .arg(&self.root)
            .output()
            .expect("binary runs");
        (out.status.code().unwrap_or(-1), out.stdout)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn every_command_is_byte_identical_at_every_job_count() {
    let p = Project::new("identical");
    for command in [
        "find",
        "unused",
        "remove-unused",
        "eq-base",
        "missing",
        "check-consistent-interpolations",
        "check-normalized",
        "health",
    ] {
        for format in ["text", "json"] {
            let (code, single) = p.run(&[command, "-f", format, "--jobs", "1"]);
            assert!(!single.is_empty(), "{command} -f {format} printed nothing");
            // The default is the core count; 2 and 8 bracket it either way.
            for jobs in ["2", "8", "16"] {
                let (c, parallel) = p.run(&[command, "-f", format, "--jobs", jobs]);
                assert_eq!(c, code, "{command} -f {format} exit code at --jobs {jobs}");
                assert_eq!(
                    String::from_utf8_lossy(&parallel),
                    String::from_utf8_lossy(&single),
                    "{command} -f {format} differs at --jobs {jobs}"
                );
            }
            // And the default pool, whatever this machine's core count is.
            let (c, default) = p.run(&[command, "-f", format]);
            assert_eq!(c, code, "{command} -f {format} exit code at the default");
            assert_eq!(
                String::from_utf8_lossy(&default),
                String::from_utf8_lossy(&single),
                "{command} -f {format} differs at the default job count"
            );
        }
    }
}

#[test]
fn zero_jobs_is_an_error_rather_than_a_deadlock() {
    let p = Project::new("zero");
    let out = Command::new(BIN)
        .args(["find", "--jobs", "0"])
        .arg("-c")
        .arg(p.root.join("config/i18n-tasks.yml"))
        .arg("--root")
        .arg(&p.root)
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--jobs must be at least 1"),
        "unexpected message: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `--jobs` sizes the pool the source scan fans out over, and `migrate-config`
/// never scans: it reads one config file and writes another. The flag belongs to
/// the commands that scan, not to every command in the binary.
#[test]
fn migrate_config_does_not_take_jobs() {
    let out = Command::new(BIN)
        .args(["migrate-config", "--jobs", "2"])
        .output()
        .expect("binary runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unexpected argument '--jobs'"),
        "unexpected message: {stderr}"
    );
    assert_eq!(out.status.code(), Some(2));
}
