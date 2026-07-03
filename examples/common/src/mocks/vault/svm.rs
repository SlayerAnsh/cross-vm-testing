//! Solana vault bindings: the Anchor program's id, instruction discriminators, and embedded `.so`.

/// The vault program's declared program id.
pub const PROGRAM_ID: &str = "GFNizKSbcjBH7aTwPyyA3vnqfksjWEfci6fgWeCJ34GB";
/// `sha256("global:initialize")[..8]`.
pub const DISC_INITIALIZE: [u8; 8] = [175, 175, 109, 31, 13, 152, 155, 237];
/// `sha256("global:deposit")[..8]`.
pub const DISC_DEPOSIT: [u8; 8] = [242, 35, 198, 137, 82, 225, 242, 182];
/// `sha256("global:withdraw")[..8]`.
pub const DISC_WITHDRAW: [u8; 8] = [183, 18, 70, 156, 148, 109, 161, 34];
/// `sha256("global:borrow")[..8]`.
pub const DISC_BORROW: [u8; 8] = [228, 253, 131, 202, 207, 116, 89, 18];
/// `sha256("global:repay")[..8]`.
pub const DISC_REPAY: [u8; 8] = [234, 103, 67, 82, 208, 234, 219, 166];
/// The compiled program, built by `make compile-solana` (`cargo-build-sbf`).
pub const PROGRAM_SO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/solana/target/deploy/vault.so"
));
