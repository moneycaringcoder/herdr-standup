//! The three renderers.
//!
//! The output *is* the product. All three take exactly one [`Digest`] and add
//! nothing to it — no second pass over git, no lookups, no interpretation. If a
//! renderer wants a number, the number belongs in the model.
//!
//! Quality bar, from the roadmap:
//!
//! - A checkout with nothing in the window is **summarised as quiet**, never
//!   omitted and never padded out with a row of zeros.
//! - Times are local and say so. Every [`Stamp`](crate::model::Stamp) carries
//!   its zone; print it.
//! - Long branch names and paths must not wrap into soup at 80 columns.
//! - The Markdown must render on GitHub even when a branch name contains
//!   characters Markdown treats specially — `feat/re[factor]`, `a|b`,
//!   `fix_*_thing` and backticks all occur in real branch names.
//! - Anything in `problems` or `notes` is always shown. A digest that hides a
//!   failure is worse than no digest.

use crate::config::{Config, Format};
use crate::model::Digest;
use crate::Result;

/// Renders in whichever format the config asks for.
pub fn render(digest: &Digest, config: &Config) -> Result<String> {
    match config.format {
        Format::Text => Ok(text(digest, config)),
        Format::Markdown => Ok(markdown(digest, config)),
        Format::Json => json(digest),
    }
}

/// Human-readable digest for a terminal. Must stay legible at 80 columns.
pub fn text(digest: &Digest, config: &Config) -> String {
    let _ = (digest, config);
    unimplemented!("render::text — owned by the presenter")
}

/// Markdown for pasting into a standup channel or a journal. This is the one
/// people judge; it should need no editing after the paste.
pub fn markdown(digest: &Digest, config: &Config) -> String {
    let _ = (digest, config);
    unimplemented!("render::markdown — owned by the presenter")
}

/// Machine-readable digest. Stable shape, versioned by
/// [`SCHEMA_VERSION`](crate::model::SCHEMA_VERSION).
pub fn json(digest: &Digest) -> Result<String> {
    Ok(serde_json::to_string_pretty(digest)?)
}
