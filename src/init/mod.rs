//! `init-config`: a config generated from the project's own layout.
//!
//! The gem's answer to "how do I start" is to copy a template
//! (`templates/config/i18n-tasks.yml`), which is the same file for every
//! project and therefore right about nothing except the defaults. This command
//! looks at the project instead:
//!
//!   * `data.read` is derived from the locale files that are actually there,
//!     and every one of them must be matched by a pattern that was emitted —
//!     checked here with [`locale_for_path`], the loader's own rule, not with a
//!     second implementation of it;
//!   * `data.write` is the first candidate target that those same patterns read
//!     back, so a later `normalize --write` cannot put keys where nothing looks
//!     for them;
//!   * `base_locale` comes from `config.i18n.default_locale` in the project's
//!     Ruby, read as text — blocker B3 applies here as everywhere else;
//!   * `search.relative_roots` keeps the gem defaults that exist, and adds a
//!     directory only when a file under it uses a relative key. That is exactly
//!     the condition under which a relative root does anything.
//!
//! Everything that cannot be detected is written out commented, so the file
//! still documents the supported surface the way the gem's template does.
//!
//! The result is parsed back with [`Config::parse`] and loaded with
//! [`Store::load`] before the command offers to write it, so the header can
//! report what the settings actually read.
//!
//! The work is in three parts, in the order the command runs them: `detect`
//! reads the project, `render` writes the file, and `verify` reads that file
//! back.
//!
//! [`locale_for_path`]: crate::data::load::locale_for_path
//! [`Config::parse`]: crate::config::Config::parse
//! [`Store::load`]: crate::data::load::Store::load

mod detect;
mod render;
mod verify;

pub use detect::{Detected, detect};
pub use render::to_text;
pub use verify::Verification;

use render::render;
use std::path::Path;
use verify::verify;

/// The file the generated config is written to.
pub const INIT_TARGET: &str = "config/i18n-tasks-rs.yml";

#[derive(Debug, Clone)]
pub struct Generated {
    pub output: String,
    pub detected: Detected,
    pub verified: Verification,
}

impl Generated {
    /// True when the generated config still needs a human.
    pub fn needs_attention(&self) -> bool {
        !self.detected.notes.is_empty() || self.detected.gem_config.is_some()
    }
}

/// Detects, renders, and reads the result back. `to` is only used in messages.
pub fn generate(root: &Path, to: &Path) -> Result<Generated, String> {
    let mut detected = detect(root);
    // The header reports what the settings read, so the file is rendered
    // twice: once to have something to load, once with the answer in it.
    let draft = render(&detected, None);
    let verified = verify(&draft, root, to);
    if let Some(err) = &verified.error {
        // Whatever the reason, it is in the error, and guessing at a fix here
        // would be wrong as often as right: a reference value in the data is
        // not a `data.read` problem.
        detected
            .notes
            .push(format!("the generated config did not load: {err}"));
    } else if verified.key_count == 0 && detected.files_seen > 0 {
        detected.notes.push(
            "the generated config loaded no keys. Check that each locale file has its \
             locale as the single top-level key."
                .into(),
        );
    }
    let output = render(&detected, Some(&verified));
    Ok(Generated {
        output,
        detected,
        verified,
    })
}
