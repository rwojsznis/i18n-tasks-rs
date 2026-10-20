use crate::cli::args::Common;
use crate::cli::exit::ExitStatus;
use crate::cli::out::{errln, out, outln};
use i18n_tasks_rs::report::normalize;
use i18n_tasks_rs::translate::openai::{DEFAULT_BASE_URL, DEFAULT_MODEL, OpenAi};
use i18n_tasks_rs::translate::{self, LocaleLeaves};

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct TranslateMissingArgs {
    #[command(flatten)]
    common: Common,
}

impl TranslateMissingArgs {
    pub(crate) fn run(&self) -> Result<ExitStatus, String> {
        let mut s = self.common.open()?;
        let pending = translate::pending(&s.cfg, &s.store, &s.locales);
        let from = s.store.base_locale.clone();
        let mut translated = Vec::with_capacity(pending.len());
        let mut failed = 0;
        if !pending.is_empty() {
            let client = client()?;
            for LocaleLeaves { locale, leaves } in pending {
                errln!("{locale}: translating {} key(s)", leaves.len());
                let result = translate::translate(
                    &leaves,
                    |key| s.store.tree(&from).is_some_and(|t| t.is_plural_node(key)),
                    |batch| client.translate(batch, &from, &locale),
                );
                for failure in &result.failures {
                    errln!(
                        "{locale}: cannot translate {}: {}",
                        failure.keys.join(", "),
                        failure.error
                    );
                    failed += failure.keys.len();
                }
                translated.push(LocaleLeaves {
                    locale,
                    leaves: result.leaves,
                });
            }
        }

        let rows = translate::rows(&translated);
        let report = translate::merge(&s.cfg, &mut s.store, translated)?;
        normalize::apply(&report)?;
        if s.json {
            outln!("{}", translate::to_json(&s, &rows)?);
        } else {
            out!("{}", translate::to_text(&rows));
        }
        if failed > 0 {
            return Err(format!(
                "{failed} key(s) were not translated and are still missing"
            ));
        }
        Ok(ExitStatus::Ok)
    }
}

fn client() -> Result<OpenAi, String> {
    let key =
        env("OPENAI_API_KEY").ok_or("translating needs an OpenAI API key in OPENAI_API_KEY")?;
    let model = env("OPENAI_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let base_url = env("OPENAI_BASE_URL").unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    Ok(OpenAi::new(&base_url, key, model))
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}
