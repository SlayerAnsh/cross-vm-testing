//! Tron chain metadata and presets.

mod info;
#[cfg(feature = "presets")]
pub mod presets;
mod sugar;

pub use info::TronChainInfo;
#[cfg(feature = "presets")]
pub use presets::{LOCAL, MAINNET, NILE, SHASTA};

#[cfg(all(test, feature = "presets"))]
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
