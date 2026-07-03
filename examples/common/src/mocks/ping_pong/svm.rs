//! Solana ping-pong bindings: the Anchor program's id, instruction discriminators, and embedded
//! `.so`.

/// The ping-pong program's declared program id.
pub const PROGRAM_ID: &str = "54ex8sgs6H3Y2NssU3CWdBhySk9q5Gqc4MMtPYTtJzC5";
/// `sha256("global:initialize")[..8]`.
pub const DISC_INITIALIZE: [u8; 8] = [175, 175, 109, 31, 13, 152, 155, 237];
/// `sha256("global:ping")[..8]`.
pub const DISC_PING: [u8; 8] = [173, 0, 94, 236, 73, 133, 225, 153];
/// `sha256("global:receive_packet")[..8]`.
pub const DISC_RECEIVE_PACKET: [u8; 8] = [63, 80, 211, 98, 33, 16, 172, 29];
/// `sha256("global:acknowledge_packet")[..8]`.
pub const DISC_ACKNOWLEDGE_PACKET: [u8; 8] = [232, 102, 184, 27, 48, 4, 54, 252];
/// The compiled program, built by `make compile-solana` (`cargo-build-sbf`).
pub const PROGRAM_SO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/solana/target/deploy/ping_pong.so"
));
