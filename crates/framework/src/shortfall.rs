//! A single account/asset funding shortfall.

/// A single account/asset funding shortfall discovered during `start()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortfall {
    /// Chain label the requirement was declared against.
    pub label: String,
    /// Account that was short.
    pub who: String,
    /// Asset that was short.
    pub asset: String,
    /// Required amount (decimal string).
    pub required: String,
    /// Actual amount held (decimal string).
    pub actual: String,
}

impl std::fmt::Display for Shortfall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}] {} short on {}: required {}, found {}",
            self.label, self.who, self.asset, self.required, self.actual
        )
    }
}
