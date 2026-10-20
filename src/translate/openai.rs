//! ref: lib/i18n/tasks/translators/openai_translator.rb

use super::language_names::language_name;
use serde_json::{Value, json};
use std::time::Duration;

pub const DEFAULT_MODEL: &str = "gpt-4o-mini";
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// ref: openai_translator.rb:12-24 after `squish` and `format`, which turn the
/// gem's `%%{` into `%{`.
const SYSTEM_PROMPT: &str = "You are a professional translator that translates content from the %{from} locale to the %{to} locale in an i18n locale array. The array has a structured format and contains multiple strings. Your task is to translate each of these strings and create a new array with the translated strings. HTML markups (enclosed in < and > characters) must not be changed under any circumstance. Variables (starting with %{ and ending with }) must not be changed under any circumstance. Keep in mind the context of all the strings for a more accurate translation. It is CRITICAL you output only the result, without any additional information, code block syntax or comments.";

const JSON_FORMAT_PROMPT: &str = "Return the translations as a JSON object with a 'translations' array containing the translated strings.";

/// ruby-openai's default `request_timeout`.
const TIMEOUT: Duration = Duration::from_secs(120);

pub struct OpenAi {
    agent: ureq::Agent,
    url: String,
    key: String,
    model: String,
}

impl std::fmt::Debug for OpenAi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAi")
            .field("url", &self.url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl OpenAi {
    pub fn new(base_url: &str, key: String, model: String) -> OpenAi {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .build()
            .into();
        OpenAi {
            agent,
            url: format!("{}/chat/completions", base_url.trim_end_matches('/')),
            key,
            model,
        }
    }

    /// # Errors
    ///
    /// The request fails, or the answer holds no `translations` list of
    /// strings.
    pub fn translate(
        &self,
        values: &[String],
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, String> {
        let mut response = self
            .agent
            .post(&self.url)
            .header("Authorization", &format!("Bearer {}", self.key))
            .send_json(request(&self.model, values, from, to))
            .map_err(|e| format!("cannot reach OpenAI: {e}"))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("cannot read OpenAI's answer: {e}"))?;
        parse(status, &body)
    }
}

pub fn request(model: &str, values: &[String], from: &str, to: &str) -> Value {
    let system = format!("{SYSTEM_PROMPT}\n{JSON_FORMAT_PROMPT}")
        .replace("%{from}", &language_name(from))
        .replace("%{to}", &language_name(to));
    json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": "Translate this array: \n\n\n" },
            { "role": "user", "content": Value::from(values).to_string() },
        ],
        "temperature": 0.0,
        "response_format": { "type": "json_object" },
    })
}

/// # Errors
///
/// The answer is an error, or does not hold a `translations` list of strings.
pub fn parse(status: u16, body: &str) -> Result<Vec<String>, String> {
    let answer: Value = serde_json::from_str(body)
        .map_err(|e| format!("OpenAI answered HTTP {status} with a body that is not JSON: {e}"))?;
    if let Some(error) = answer.get("error").filter(|e| !e.is_null()) {
        let message = error["message"]
            .as_str()
            .map_or_else(|| error.to_string(), str::to_string);
        return Err(format!("AI error: {message}"));
    }
    if !(200..300).contains(&status) {
        return Err(format!("OpenAI answered HTTP {status}"));
    }
    let content = answer["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("OpenAI's answer holds no message content")?;
    let content: Value =
        serde_json::from_str(content).map_err(|e| format!("OpenAI's message is not JSON: {e}"))?;
    content["translations"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .ok_or_else(|| "OpenAI's message holds no `translations` list of strings".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_is_the_gems_three_messages() {
        let body = request("gpt-4o", &["Hello, X__0".into(), "Bye".into()], "en", "fi");
        assert_eq!(body["model"], "gpt-4o");
        let system = body["messages"][0]["content"].as_str().unwrap();
        assert!(system.starts_with(
            "You are a professional translator that translates content from the English locale to the Finnish locale"
        ));
        assert!(system.contains("Variables (starting with %{ and ending with })"));
        assert!(system.ends_with(&format!("comments.\n{JSON_FORMAT_PROMPT}")));
        assert_eq!(
            body["messages"][1]["content"],
            "Translate this array: \n\n\n"
        );
        assert_eq!(body["messages"][2]["content"], r#"["Hello, X__0","Bye"]"#);
    }

    #[test]
    fn an_unknown_locale_is_named_as_written() {
        let body = request("m", &[], "en", "fi-easy");
        let system = body["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("to the fi-easy locale"), "{system}");
    }

    #[test]
    fn parse_reads_the_translations_list() {
        let content = json!({ "translations": ["Hei", "Moi"] }).to_string();
        let body = json!({ "choices": [{ "message": { "content": content } }] }).to_string();
        assert_eq!(parse(200, &body).unwrap(), ["Hei", "Moi"]);
    }

    #[test]
    fn parse_reports_the_api_error_message() {
        let body = r#"{"error":{"message":"Incorrect API key provided"}}"#;
        assert_eq!(
            parse(401, body).unwrap_err(),
            "AI error: Incorrect API key provided"
        );
    }

    #[test]
    fn parse_rejects_an_answer_without_a_list_of_strings() {
        for content in [r#"{"translations": [1]}"#, r#"{"text": "Hei"}"#, "Hei"] {
            let body = json!({ "choices": [{ "message": { "content": content } }] }).to_string();
            assert!(parse(200, &body).is_err(), "{content}");
        }
        assert!(parse(502, "<html>Bad gateway</html>").is_err());
    }
}
