//! Declarative TOML/JSON run-config schema for `harness-core`: parse,
//! interpolate, merge, typed deserialize, validate. Pure data, no runtime
//! dependencies. Domain layers extend it via [`ConfigExt`]; see the cross-vm
//! crates for a worked example.
