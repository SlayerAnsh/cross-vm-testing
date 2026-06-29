//! Uniform funding dispatch and the deferred requirements it produces.

mod fund_target;
mod pending;

pub use fund_target::FundTarget;
pub(crate) use pending::Pending;
