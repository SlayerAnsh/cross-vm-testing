//! Typed CosmWasm JSON messages for provider method generics.

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Any CosmWasm JSON message passed through the provider.
pub trait CwSerde: Serialize + DeserializeOwned + Send + Sync + std::fmt::Debug {}
impl<T> CwSerde for T where T: Serialize + DeserializeOwned + Send + Sync + std::fmt::Debug {}
