//! Validation errors for owner-supplied planning input.
//!
//! Watchlists and baskets are typed by hand (config file, admin bot), so the
//! crate validates them explicitly and reports **every** problem at once
//! rather than failing on the first one.

use rust_decimal::Decimal;

/// A problem with owner-supplied planning input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanningError {
    /// A monetary field was negative.
    #[error("{field} must not be negative (got {value})")]
    NegativeAmount { field: &'static str, value: Decimal },

    /// An identifier that must be meaningful was blank.
    #[error("{field} must not be empty")]
    BlankIdentifier { field: &'static str },

    /// The same shop appeared twice in one basket.
    #[error("shop `{shop_id}` appears more than once in the basket")]
    DuplicateShop { shop_id: String },

    /// The same watch item id was registered twice.
    #[error("watch item `{id}` is already in the watchlist")]
    DuplicateWatchItem { id: String },

    /// A basket line had zero quantity.
    #[error("item `{title}` must have a quantity of at least 1")]
    ZeroQuantity { title: String },

    /// A shop's stated subtotal disagrees with the sum of its listed items.
    ///
    /// Not auto-corrected: the owner decides which number is right.
    #[error("shop `{shop_id}` subtotal {stated} does not match its listed items ({computed})")]
    SubtotalMismatch {
        shop_id: String,
        stated: Decimal,
        computed: Decimal,
    },

    /// The basket has nothing to plan against.
    #[error("basket contains no shops")]
    EmptyBasket,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_offending_field() {
        let err = PlanningError::NegativeAmount {
            field: "planned_spend",
            value: Decimal::from(-5),
        };
        assert!(err.to_string().contains("planned_spend"));
        assert!(err.to_string().contains("-5"));

        let err = PlanningError::SubtotalMismatch {
            shop_id: "shop-1".into(),
            stated: Decimal::from(100),
            computed: Decimal::from(120),
        };
        assert!(err.to_string().contains("shop-1"));
        assert!(err.to_string().contains("120"));
    }
}
