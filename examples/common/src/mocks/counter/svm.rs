//! Solana counter bindings: the Anchor program's declared id, instruction discriminators, and
//! embedded `.so`.

/// The counter program's declared program id.
pub const PROGRAM_ID: &str = "7TSiNYMVrY4CtSzE4MjAWzhNGpYWs9m2kdaFPuR8ZhJK";
/// `sha256("global:initialize")[..8]`.
pub const DISC_INITIALIZE: [u8; 8] = [175, 175, 109, 31, 13, 152, 155, 237];
/// `sha256("global:increment")[..8]`.
pub const DISC_INCREMENT: [u8; 8] = [11, 18, 104, 9, 104, 174, 59, 33];
/// The compiled program, built by `make compile-solana` (`cargo-build-sbf`).
pub const PROGRAM_SO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/solana/target/deploy/counter.so"
));
