//! `translate-missing` end to end, against a fake OpenAI server on a local
//! socket. `OPENAI_BASE_URL` points the binary at it.
//!
//! ref: lib/i18n/tasks/command/commands/missing.rb:40-57
//! ref: lib/i18n/tasks/translators/openai_translator.rb

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

const BIN: &str = env!("CARGO_BIN_EXE_i18n-tasks-rs");

const CONFIG: &str = "base_locale: en\nlocales: [en, de, fi]\ndata:\n  read:\n    - config/locales/%{locale}.yml\n    - config/locales/**/*.%{locale}.yml\n  write:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\nignore_missing:\n  - secret.*\n";

struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let root = std::env::temp_dir().join(format!("i18n-tasks-rs-translate-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("app")).unwrap();
        let project = Project { root };
        project.write("config/i18n-tasks.yml", CONFIG);
        project
    }

    fn write(&self, rel: &str, contents: &str) -> &Project {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
        self
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap_or_default()
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .arg("-c")
            .arg(self.root.join("config/i18n-tasks.yml"))
            .arg("--root")
            .arg(&self.root)
            .env_remove("OPENAI_API_KEY")
            .env_remove("OPENAI_MODEL")
            .env_remove("OPENAI_BASE_URL");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("binary runs");
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

#[derive(Debug, Clone)]
struct Request {
    authorization: String,
    body: serde_json::Value,
}

const FAILING_VALUE: &str = "FAIL";

/// A chat-completions endpoint that "translates" each string by prefixing the
/// target language name from the system prompt. A batch that holds
/// `FAILING_VALUE` gets an HTTP 500.
struct FakeOpenAi {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl FakeOpenAi {
    fn start() -> FakeOpenAi {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0usize;
                let mut authorization = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let line = line.trim_end();
                    if line.is_empty() {
                        break;
                    }
                    let (name, value) = line.split_once(':').unwrap_or((line, ""));
                    match name.to_ascii_lowercase().as_str() {
                        "content-length" => length = value.trim().parse().unwrap(),
                        "authorization" => authorization = value.trim().to_string(),
                        _ => {}
                    }
                }
                let mut body = vec![0u8; length];
                reader.read_exact(&mut body).unwrap();
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                seen.lock().unwrap().push(Request {
                    authorization,
                    body: body.clone(),
                });
                let (status, reply) = reply_to(&body);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        FakeOpenAi { url, requests }
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

fn reply_to(body: &serde_json::Value) -> (&'static str, String) {
    let system = body["messages"][0]["content"].as_str().unwrap();
    let language = system
        .split(" locale to the ")
        .nth(1)
        .and_then(|rest| rest.split(" locale").next())
        .unwrap();
    let values: Vec<String> =
        serde_json::from_str(body["messages"][2]["content"].as_str().unwrap()).unwrap();
    if values.iter().any(|v| v == FAILING_VALUE) {
        return (
            "500 Internal Server Error",
            r#"{"error":{"message":"the model is overloaded"}}"#.to_string(),
        );
    }
    let translations: Vec<String> = values.iter().map(|v| format!("{language}: {v}")).collect();
    let content = serde_json::json!({ "translations": translations }).to_string();
    let reply = serde_json::json!({
        "choices": [{ "message": { "role": "assistant", "content": content } }]
    });
    ("200 OK", reply.to_string())
}

const EN: &str = "en:\n  greeting: Hello, %{name}!\n  apples:\n    one: One apple\n    other: '%{count} apples'\n  days: [Monday, Tuesday]\n  limit: 5\n  secret:\n    token: hidden\n";

fn project(name: &str) -> Project {
    let p = Project::new(name);
    p.write("config/locales/en.yml", EN)
        .write(
            "config/locales/de.yml",
            "de:\n  greeting: Hallo, %{name}!\n",
        )
        .write("config/locales/fi.yml", "fi:\n  limit: 5\n");
    p
}

#[test]
fn it_reports_the_keys_it_translated() {
    let p = project("report");
    let server = FakeOpenAi::start();
    let env = [
        ("OPENAI_API_KEY", "sk-test"),
        ("OPENAI_BASE_URL", server.url.as_str()),
    ];
    let (code, out) = p.run(&["translate-missing", "de"], &env);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.lines()
            .any(|l| l.starts_with("de ") && l.contains("apples.one")),
        "{out}"
    );
    assert!(
        !out.contains("secret.token"),
        "ignore_missing applies: {out}"
    );

    let (code, out) = p.run(&["translate-missing", "-f", "json", "fi"], &env);
    assert_eq!(code, 0, "{out}");
    let stdout = &out[out.find('{').unwrap()..=out.rfind('}').unwrap()];
    let json: serde_json::Value = serde_json::from_str(stdout).unwrap();
    assert_eq!(json["check"], "translate-missing");
    let keys: Vec<&str> = json["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["apples.one", "apples.other", "days", "greeting"]);
}

#[test]
fn it_writes_the_missing_keys_and_the_result_passes_the_checks() {
    let p = project("write");
    let server = FakeOpenAi::start();
    let env = [
        ("OPENAI_API_KEY", "sk-test"),
        ("OPENAI_MODEL", "gpt-4o"),
        ("OPENAI_BASE_URL", server.url.as_str()),
    ];
    let (code, out) = p.run(&["translate-missing", "de", "fi"], &env);
    assert_eq!(code, 0, "{out}");

    let de = p.read("config/locales/de.yml");
    assert!(de.contains("one: 'German: One apple'"), "{de}");
    assert!(de.contains("other: 'German: %{count} apples'"), "{de}");
    assert!(de.contains("- 'German: Monday'"), "{de}");
    assert!(de.contains("limit: 5"), "{de}");
    assert!(de.contains("greeting: Hallo, %{name}!"), "{de}");
    assert!(!de.contains("secret"), "{de}");
    let fi = p.read("config/locales/fi.yml");
    assert!(fi.contains("greeting: 'Finnish: Hello, %{name}!'"), "{fi}");

    let requests = server.requests();
    assert_eq!(requests.len(), 2, "one batch per locale");
    for r in &requests {
        assert_eq!(r.authorization, "Bearer sk-test");
        assert_eq!(r.body["model"], "gpt-4o");
        assert_eq!(r.body["temperature"], 0.0);
        assert_eq!(r.body["response_format"]["type"], "json_object");
    }
    let sent: Vec<String> =
        serde_json::from_str(requests[0].body["messages"][2]["content"].as_str().unwrap()).unwrap();
    assert!(
        sent.contains(&"X__0 apples".to_string()),
        "interpolations travel as placeholders: {sent:?}"
    );

    for check in ["check-normalized", "check-consistent-interpolations"] {
        let (code, out) = p.run(&[check, "de", "fi"], &[]);
        assert_eq!(code, 0, "{check}: {out}");
    }
    let (code, out) = p.run(&["translate-missing"], &env);
    assert_eq!(code, 0, "{out}");
    assert_eq!(server.requests().len(), 2, "nothing is left to translate");
}

#[test]
fn the_model_defaults_to_gpt_4o_mini() {
    let p = project("default-model");
    let server = FakeOpenAi::start();
    let env = [
        ("OPENAI_API_KEY", "sk-test"),
        ("OPENAI_MODEL", ""),
        ("OPENAI_BASE_URL", server.url.as_str()),
    ];
    let (code, out) = p.run(&["translate-missing", "fi"], &env);
    assert_eq!(code, 0, "{out}");
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "only the locale asked for");
    assert_eq!(requests[0].body["model"], "gpt-4o-mini");
    assert_eq!(
        p.read("config/locales/de.yml"),
        "de:\n  greeting: Hallo, %{name}!\n"
    );
}

#[test]
fn without_a_key_it_fails_before_touching_anything() {
    let p = project("no-key");
    let server = FakeOpenAi::start();
    for key in [None, Some("  ")] {
        let mut env = vec![("OPENAI_BASE_URL", server.url.as_str())];
        env.extend(key.map(|k| ("OPENAI_API_KEY", k)));
        let (code, out) = p.run(&["translate-missing"], &env);
        assert_eq!(code, 2, "{out}");
        assert!(out.contains("OPENAI_API_KEY"), "{out}");
    }
    assert!(server.requests().is_empty());
    assert_eq!(p.read("config/locales/fi.yml"), "fi:\n  limit: 5\n");
}

#[test]
fn with_nothing_missing_it_needs_no_key() {
    let p = project("nothing-missing");
    p.write("config/locales/en.yml", "en:\n  limit: 5\n");
    let (code, out) = p.run(&["translate-missing", "fi"], &[]);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn files_that_receive_no_key_are_left_as_they_are() {
    let p = project("untouched");
    let untouched = "de:\n  only_here:   \"Nur hier\"\n";
    p.write("config/locales/extra/more.de.yml", untouched);
    let server = FakeOpenAi::start();
    let env = [
        ("OPENAI_API_KEY", "sk-test"),
        ("OPENAI_BASE_URL", server.url.as_str()),
    ];
    let (code, out) = p.run(&["translate-missing", "de"], &env);
    assert_eq!(code, 0, "{out}");
    assert!(
        p.read("config/locales/de.yml")
            .contains("German: One apple")
    );
    assert_eq!(p.read("config/locales/extra/more.de.yml"), untouched);
}

#[test]
fn an_untouched_file_holding_two_locales_does_not_block_the_write() {
    let p = project("shared-file");
    p.write(
        "config/i18n-tasks.yml",
        "base_locale: en\nlocales: [en, de, fi]\ndata:\n  read:\n    - config/locales/%{locale}.yml\n    - config/locales/shared.yml\nsearch:\n  paths: [app/]\n",
    )
    .write("config/locales/shared.yml", "de:\n  s: S\nfi:\n  s: S\n");
    let server = FakeOpenAi::start();
    let env = [
        ("OPENAI_API_KEY", "sk-test"),
        ("OPENAI_BASE_URL", server.url.as_str()),
    ];
    let (code, out) = p.run(&["translate-missing", "de"], &env);
    assert_eq!(code, 0, "{out}");
    assert!(
        p.read("config/locales/de.yml")
            .contains("German: One apple")
    );
    assert_eq!(
        p.read("config/locales/shared.yml"),
        "de:\n  s: S\nfi:\n  s: S\n"
    );
}

#[test]
fn a_project_inside_a_directory_named_like_a_locale_keeps_its_root() {
    let p = Project::new("root/en");
    p.write(
        "config/i18n-tasks.yml",
        "base_locale: en\nlocales: [en, de]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    )
    .write("config/locales/en.yml", "en:\n  greeting: Hello\n");
    let server = FakeOpenAi::start();
    let env = [
        ("OPENAI_API_KEY", "sk-test"),
        ("OPENAI_BASE_URL", server.url.as_str()),
    ];
    let (code, out) = p.run(&["translate-missing", "de"], &env);
    assert_eq!(code, 0, "{out}");
    assert!(
        p.read("config/locales/de.yml").contains("German: Hello"),
        "{out}"
    );
    assert!(!p.root.with_file_name("de").exists());
}

#[test]
fn a_new_key_is_never_written_into_an_external_file() {
    for shared in ["en:\n  brand: Acme\n", "de:\n  brand: Acme\n"] {
        let p = project("external-file");
        p.write(
            "config/i18n-tasks.yml",
            "base_locale: en\nlocales: [en, de]\ndata:\n  router: pattern_router\n  read:\n    - config/locales/%{locale}.yml\n  external:\n    - config/locales/shared.yml\n  write:\n    - config/locales/shared.yml\nsearch:\n  paths: [app/]\n",
        )
        .write("config/locales/shared.yml", shared);
        let server = FakeOpenAi::start();
        let env = [
            ("OPENAI_API_KEY", "sk-test"),
            ("OPENAI_BASE_URL", server.url.as_str()),
        ];
        let (code, out) = p.run(&["translate-missing", "de"], &env);
        assert_eq!(code, 2, "{shared}: {out}");
        assert!(out.contains("shared.yml"), "{out}");
        assert_eq!(p.read("config/locales/shared.yml"), shared);
    }
}

#[test]
fn a_new_key_is_never_written_into_another_locales_file() {
    let p = project("foreign-file");
    let en = "en:\n  greeting: Hello\n";
    p.write(
        "config/i18n-tasks.yml",
        "base_locale: en\nlocales: [en, de]\ndata:\n  read:\n    - config/locales/app_%{locale}.yml\n  write:\n    - config/locales/app_%{locale}.yml\nsearch:\n  paths: [app/]\n",
    )
    .write("config/locales/app_en.yml", en);
    let server = FakeOpenAi::start();
    let env = [
        ("OPENAI_API_KEY", "sk-test"),
        ("OPENAI_BASE_URL", server.url.as_str()),
    ];
    let (code, out) = p.run(&["translate-missing", "de"], &env);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("app_en.yml"), "{out}");
    assert_eq!(p.read("config/locales/app_en.yml"), en);
}

#[test]
fn the_pattern_router_places_new_keys_and_moves_no_existing_one() {
    let p = project("pattern-router");
    p.write(
        "config/i18n-tasks.yml",
        "base_locale: en\nlocales: [en, de]\ndata:\n  router: pattern_router\n  read:\n    - config/locales/%{locale}.yml\n    - config/locales/*.%{locale}.yml\n  write:\n    - ['admin.*', 'config/locales/admin.%{locale}.yml']\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    )
    .write(
        "config/locales/en.yml",
        "en:\n  admin:\n    x: Admin\n    y: Settings\n  hi: Hi\n",
    )
    .write("config/locales/de.yml", "de:\n  admin:\n    x: Verwaltung\n");
    let server = FakeOpenAi::start();
    let env = [
        ("OPENAI_API_KEY", "sk-test"),
        ("OPENAI_BASE_URL", server.url.as_str()),
    ];
    let (code, out) = p.run(&["translate-missing", "de"], &env);
    assert_eq!(code, 0, "{out}");
    let de = p.read("config/locales/de.yml");
    assert!(
        de.contains("x: Verwaltung"),
        "an existing key stays put: {de}"
    );
    assert!(de.contains("hi: 'German: Hi'"), "{de}");
    let admin = p.read("config/locales/admin.de.yml");
    assert!(admin.contains("'y': 'German: Settings'"), "{admin}");
    assert!(!admin.contains("Verwaltung"), "nothing is copied: {admin}");
}

#[test]
fn a_failed_batch_is_reported_and_its_keys_stay_missing() {
    let p = project("failed-batch");
    p.write(
        "config/locales/en.yml",
        &format!("en:\n  greeting: Hello, %{{name}}!\n  broken: {FAILING_VALUE}\n  ok: Fine\n"),
    )
    .write(
        "config/locales/de.yml",
        "de:\n  greeting: Hallo, %{name}!\n  broken: Kaputt\n",
    );
    let server = FakeOpenAi::start();
    let env = [
        ("OPENAI_API_KEY", "sk-test"),
        ("OPENAI_BASE_URL", server.url.as_str()),
    ];
    let (code, out) = p.run(&["translate-missing", "de", "fi"], &env);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("the model is overloaded"), "{out}");
    let de = p.read("config/locales/de.yml");
    assert!(
        de.contains("ok: 'German: Fine'"),
        "a good batch is still written: {de}"
    );
    let fi = p.read("config/locales/fi.yml");
    assert!(
        !fi.contains(FAILING_VALUE),
        "the source text is never written: {fi}"
    );
    assert!(
        !fi.contains("greeting"),
        "the batch failed as a whole: {fi}"
    );
}
