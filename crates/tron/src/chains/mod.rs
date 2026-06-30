//! Tron chain metadata and presets.

mod info;
pub mod presets;
mod sugar;

pub use info::TronChainInfo;
pub use presets::{LOCAL, MAINNET, NILE, SHASTA};

#[cfg(test)]
mod tests {
    use super::*;
    use cross_vm_core::{ChainKind, ChainSpec};

    #[test]
    fn mainnet_is_tron() {
        assert_eq!(MAINNET.kind(), ChainKind::Tron);
        assert_eq!(MAINNET.native_symbol(), "TRX");
        assert_eq!(MAINNET.numeric_id(), 728126428);
    }
}
