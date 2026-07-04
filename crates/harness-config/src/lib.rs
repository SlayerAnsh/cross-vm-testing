//! Declarative TOML/JSON run-config schema for `harness-core`: parse,
//! interpolate, merge, typed deserialize, validate. Pure data, no runtime
//! dependencies. Domain layers extend it via [`ConfigExt`]; see the cross-vm
//! crates for a worked example.

mod duration;
mod interpolate;
mod seed;
mod value;

pub use duration::{humantime_duration, humantime_opt};
pub use interpolate::interpolate_value;
pub use seed::SeedSpec;
pub use value::{Doc, DocMap};

/// Errors returned while loading or parsing a config document. (Extended in later tasks.)
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A `${VAR}` reference had no value and no `:-default` fallback. Never carries the
    /// surrounding string value, since it may hold an RPC secret.
    #[error(
        "undefined variable `{var}` referenced at `{path}` (set the environment variable, or add a `:-default` fallback)"
    )]
    MissingVar {
        /// The variable name, exactly as written inside `${...}`.
        var: String,
        /// The TOML path of the string value that referenced it (e.g. `chain[1].rpc_url`).
        path: String,
    },
    /// A `${...}` interpolation expression was malformed (e.g. an unterminated `${`).
    #[error("invalid interpolation expression at `{path}`: {message}")]
    Interpolation {
        /// The TOML path of the offending string value.
        path: String,
        /// A description of what was malformed; never echoes the value's contents.
        message: String,
    },
}
