//! Where the wall clock goes, stage by stage. Dev tooling, not part of the CLI
//! surface.
//!
//!   cargo run --release --example phase_timing -- <config> <project-root>

use i18n_tasks_rs::config::Config;
use i18n_tasks_rs::data::load::Store;
use i18n_tasks_rs::discover::Finder;
use i18n_tasks_rs::used::UsedKeys;
use std::path::Path;
use std::time::Instant;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let t0 = Instant::now();
    let cfg = Config::load(Path::new(&args[1]), Some(Path::new(&args[2])))?;
    println!("config load      {:>8.0?}", t0.elapsed());

    let t = Instant::now();
    let store = Store::load(&cfg)?;
    println!(
        "data load        {:>8.0?}  ({} keys)",
        t.elapsed(),
        store.locales.len()
    );

    let t = Instant::now();
    let found = Finder::new(&cfg)?.discover();
    println!(
        "discover         {:>8.0?}  ({} files, {} prefiltered)",
        t.elapsed(),
        found.files.len(),
        found.prefiltered
    );

    let t = Instant::now();
    let used = UsedKeys::scan(&cfg)?;
    println!(
        "discover + scan  {:>8.0?}  ({} keys)",
        t.elapsed(),
        used.keys.len()
    );
    println!("total            {:>8.0?}", t0.elapsed());
    Ok(())
}
