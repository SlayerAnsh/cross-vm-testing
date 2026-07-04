//! Config-driven CLI, harness registry, and run pipeline over `harness-core`
//! and `harness-config`. Use raw via [`GenericDomain`], or implement
//! [`CliDomain`] to add domain config sections, CLI flags, and a custom setup
//! request type; see the cross-vm framework crate for a worked example.
