//! Type-safe identifiers.
//!
//! Ids are opaque strings. The domain never generates them: the application/infra layers mint
//! them (UUID v7 recommended) so the domain stays free of randomness and clocks.

use std::fmt;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// Wraps an already-minted id value.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// The id as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

define_id!(
    /// Identifies a [`crate::Session`].
    SessionId
);
define_id!(
    /// Identifies a captured [`crate::Frame`].
    FrameId
);
define_id!(
    /// Identifies a generated [`crate::Note`].
    NoteId
);
define_id!(
    /// Identifies an [`crate::ActionItem`].
    ActionItemId
);
define_id!(
    /// Identifies a [`crate::Variable`].
    VariableId
);
define_id!(
    /// Identifies a secret in the credentials store (the value lives in the OS vault).
    SecretId
);
define_id!(
    /// Identifies a [`crate::ProviderProfile`].
    ProviderProfileId
);

#[cfg(test)]
mod tests {
    use super::{FrameId, SessionId};

    #[test]
    fn ids_round_trip_their_value() {
        let id = SessionId::new("abc");
        assert_eq!(id.as_str(), "abc");
        assert_eq!(id.to_string(), "abc");
        assert_eq!(id, SessionId::new(String::from("abc")));
    }

    #[test]
    fn ids_of_different_entities_are_distinct_types() {
        fn takes_session(_: &SessionId) {}
        takes_session(&SessionId::new("x"));
        let _frame = FrameId::new("x");
    }
}
