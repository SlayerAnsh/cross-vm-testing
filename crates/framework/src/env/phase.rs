//! Typestate markers for the environment lifecycle.

/// Setup phase: chains and funding can be declared.
pub struct Setup;

/// Running phase: only chain execution is allowed.
pub struct Running;
