//! Tron precompiles: secp256k1 multisignature verification plus the TVM-relocated
//! standard precompiles.
//!
//! Tron diverges from Ethereum in two ways relevant here:
//!   1. It adds a `validatemultisign` precompile that recovers and counts the signers of a
//!      32-byte digest, used by TRC-60 multi-signature accounts.
//!      Source: <https://github.com/tronprotocol/tips/blob/master/tip-60.md>
//!   2. TIP-272 relocates `ripemd160` (0x03 -> 0x20003) and `blake2f` (0x09 -> 0x20009) so the
//!      0x03/0x09 slots stay free for future use, and registers `validatemultisign` at 0x0a.
//!      Source: <https://github.com/tronprotocol/tips/blob/master/tip-272.md>
//!
//! Tron accounts use secp256k1 (the same curve as Ethereum), NOT ed25519, so signer recovery is
//! ordinary ECDSA public-key recovery from a prehashed digest.

use alloy_primitives::{keccak256, Address};
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};

use revm::precompile::{
    blake2, hash, u64_to_address, PrecompileHalt, PrecompileId, PrecompileResult,
};
use revm::precompile::{call_eth_precompile, Precompile, PrecompileOutput, Precompiles};

/// Maximum number of signatures considered for a single multisig verification.
///
/// TRC-60 permissions cap an account's active key set at 5 keys, so signatures beyond the fifth
/// can never raise the recovered-signer count and are ignored.
/// Source: <https://github.com/tronprotocol/tips/blob/master/tip-60.md>
const MAX_MULTISIGN_KEYS: usize = 5;

/// TIP-272 address for the relocated `ripemd160` precompile (was 0x03).
const RIPEMD160_TVM_ADDR: u64 = 0x20003;
/// TIP-272 address for the relocated `blake2f` precompile (was 0x09).
const BLAKE2F_TVM_ADDR: u64 = 0x20009;
/// `validatemultisign` precompile address.
const VALIDATE_MULTISIGN_ADDR: u64 = 0x0a;

/// Recover the EVM addresses that signed `content` (a 32-byte digest / prehash).
///
/// Each signature is the 65-byte `r(32) || s(32) || v(1)` form. `v` is a recovery id, accepted
/// either raw (`0`/`1`) or in the Ethereum `27`/`28` convention. Signatures that fail to parse or
/// recover are skipped. At most [`MAX_MULTISIGN_KEYS`] signatures are processed (TRC-60).
///
/// Recovery follows Tron account derivation: the recovered secp256k1 public key is taken in its
/// uncompressed SEC1 form, the `0x04` tag is dropped, the 64-byte body is `keccak256`-hashed, and
/// the low 20 bytes form the address.
/// Source: <https://github.com/tronprotocol/tips/blob/master/tip-60.md>
pub fn validate_multisign(content: [u8; 32], sigs: &[[u8; 65]]) -> Vec<Address> {
    let mut signers = Vec::new();

    for sig in sigs.iter().take(MAX_MULTISIGN_KEYS) {
        if let Some(addr) = recover_signer(&content, sig) {
            signers.push(addr);
        }
    }

    signers
}

/// Recover one signer address from a 65-byte signature over `prehash`, or `None` on any failure.
fn recover_signer(prehash: &[u8; 32], sig: &[u8; 65]) -> Option<Address> {
    let signature = Signature::from_slice(&sig[..64]).ok()?;

    // Normalise the recovery id: accept both the raw 0/1 form and the 27/28 Ethereum convention.
    let raw_v = sig[64];
    let recid_byte = if raw_v >= 27 { raw_v - 27 } else { raw_v };
    let recovery_id = RecoveryId::from_byte(recid_byte)?;

    let verifying_key =
        VerifyingKey::recover_from_prehash(prehash, &signature, recovery_id).ok()?;
    Some(address_from_verifying_key(&verifying_key))
}

/// keccak256 of the 64-byte uncompressed public-key body, low 20 bytes as an EVM address.
fn address_from_verifying_key(vk: &VerifyingKey) -> Address {
    let encoded = vk.to_encoded_point(false);
    // `as_bytes()` is the 65-byte SEC1 form `0x04 || X || Y`; drop the tag for the keccak input.
    let body = &encoded.as_bytes()[1..];
    let hash = keccak256(body);
    Address::from_slice(&hash[12..])
}

/// `ripemd160` body, re-exposed at the TIP-272 TVM address.
fn ripemd160_relocated(input: &[u8], gas_limit: u64, reservoir: u64) -> PrecompileResult {
    Ok(call_eth_precompile(
        hash::ripemd160_run,
        input,
        gas_limit,
        reservoir,
    ))
}

/// `blake2f` body, re-exposed at the TIP-272 TVM address.
fn blake2f_relocated(input: &[u8], gas_limit: u64, reservoir: u64) -> PrecompileResult {
    Ok(call_eth_precompile(
        blake2::run,
        input,
        gas_limit,
        reservoir,
    ))
}

/// `validatemultisign` precompile (address 0x0a).
///
/// The on-chain calldata is ABI-encoded `(address, uint256, bytes32, bytes[])`; decoding it and
/// folding in permission weights is wired together with the mock provider, where the account
/// permission set is available. The signer-recovery core lives in [`validate_multisign`] and is
/// unit-tested directly. Until the decoder lands this charges a flat ecrecover-class cost and
/// returns ABI-encoded `false`.
/// Source: <https://github.com/tronprotocol/tips/blob/master/tip-60.md>
fn validate_multisign_precompile(input: &[u8], gas_limit: u64, reservoir: u64) -> PrecompileResult {
    // ecrecover is 3000 gas; charge once per candidate signature (capped at the TRC-60 key limit).
    const PER_SIG_COST: u64 = 3000;
    let sig_estimate = (input.len() / 65).min(MAX_MULTISIGN_KEYS) as u64;
    let cost = PER_SIG_COST.saturating_mul(sig_estimate.max(1));

    if cost > gas_limit {
        return Ok(PrecompileOutput::halt(PrecompileHalt::OutOfGas, reservoir));
    }

    // TODO(tron): decode the ABI calldata and call `validate_multisign`, weighing recovered
    // signers against the account permission set (wired with the mock provider).
    Ok(PrecompileOutput::new(
        cost,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
        reservoir,
    ))
}

/// The TVM precompile set: Cancun, with TIP-272 relocations and `validatemultisign` registered.
///
/// `ripemd160` moves from 0x03 to 0x20003 and `blake2f` from 0x09 to 0x20009; their original slots
/// are left empty. `validatemultisign` is registered at 0x0a.
/// Source: <https://github.com/tronprotocol/tips/blob/master/tip-272.md>
pub fn tron_precompiles() -> Precompiles {
    // Remove the standard ripemd160 (0x03) and blake2f (0x09) entries by address difference.
    let mut relocated_originals = Precompiles::default();
    relocated_originals.extend([hash::RIPEMD160, blake2::FUN]);
    let mut precompiles = Precompiles::cancun().difference(&relocated_originals);

    precompiles.extend([
        Precompile::new(
            PrecompileId::Ripemd160,
            u64_to_address(RIPEMD160_TVM_ADDR),
            ripemd160_relocated,
        ),
        Precompile::new(
            PrecompileId::Blake2F,
            u64_to_address(BLAKE2F_TVM_ADDR),
            blake2f_relocated,
        ),
        Precompile::new(
            PrecompileId::custom("validatemultisign"),
            u64_to_address(VALIDATE_MULTISIGN_ADDR),
            validate_multisign_precompile,
        ),
    ]);

    precompiles
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey;

    /// Deterministic, non-zero secp256k1 key seeded from a byte (avoids a `rand` dev-dependency).
    fn signing_key(seed: u8) -> SigningKey {
        let mut bytes = [1u8; 32];
        bytes[31] = seed;
        SigningKey::from_slice(&bytes).expect("valid scalar")
    }

    /// The Tron/EVM address for a signing key, derived independently of `validate_multisign`.
    fn expected_address(sk: &SigningKey) -> Address {
        let vk = sk.verifying_key();
        let encoded = vk.to_encoded_point(false);
        let hash = keccak256(&encoded.as_bytes()[1..]);
        Address::from_slice(&hash[12..])
    }

    /// Build the 65-byte `r || s || v` signature for `digest` under `sk`.
    fn sign(sk: &SigningKey, digest: &[u8; 32]) -> [u8; 65] {
        let (sig, recid) = sk
            .sign_prehash_recoverable(digest)
            .expect("prehash signing");
        let mut out = [0u8; 65];
        out[..64].copy_from_slice(&sig.to_bytes());
        out[64] = recid.to_byte();
        out
    }

    #[test]
    fn recovers_single_signer() {
        let sk = signing_key(7);
        let digest = [9u8; 32];
        let sig = sign(&sk, &digest);

        let recovered = validate_multisign(digest, &[sig]);

        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0], expected_address(&sk));
    }

    #[test]
    fn accepts_eth_style_v() {
        let sk = signing_key(11);
        let digest = [3u8; 32];
        let mut sig = sign(&sk, &digest);
        // Shift the recovery id into the 27/28 convention; recovery must still succeed.
        sig[64] += 27;

        let recovered = validate_multisign(digest, &[sig]);

        assert_eq!(recovered, vec![expected_address(&sk)]);
    }

    #[test]
    fn caps_at_five() {
        let digest = [42u8; 32];
        // Six distinct, valid signatures; only the first five are processed.
        let sigs: Vec<[u8; 65]> = (0..6).map(|i| sign(&signing_key(i + 1), &digest)).collect();

        let recovered = validate_multisign(digest, &sigs);

        assert!(recovered.len() <= MAX_MULTISIGN_KEYS);
        assert_eq!(recovered.len(), MAX_MULTISIGN_KEYS);
    }

    #[test]
    fn skips_unrecoverable_signature() {
        let sk = signing_key(5);
        let digest = [1u8; 32];
        let good = sign(&sk, &digest);
        let garbage = [0xffu8; 65];

        let recovered = validate_multisign(digest, &[garbage, good]);

        assert_eq!(recovered, vec![expected_address(&sk)]);
    }

    #[test]
    fn precompiles_relocate_and_register() {
        let p = tron_precompiles();
        assert!(!p.is_empty());

        // ecrecover stays at its standard slot.
        assert!(p.get(&u64_to_address(0x01)).is_some());

        // ripemd160 and blake2f vacate their Ethereum slots...
        assert!(p.get(&u64_to_address(0x03)).is_none());
        assert!(p.get(&u64_to_address(0x09)).is_none());

        // ...and reappear at the TIP-272 addresses, with validatemultisign at 0x0a.
        assert!(p.get(&u64_to_address(RIPEMD160_TVM_ADDR)).is_some());
        assert!(p.get(&u64_to_address(BLAKE2F_TVM_ADDR)).is_some());
        assert!(p.get(&u64_to_address(VALIDATE_MULTISIGN_ADDR)).is_some());
    }
}
