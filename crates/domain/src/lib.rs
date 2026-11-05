//! Domain layer: entities and rules. Depends on nothing but `std`.
//!
//! The model is deliberately not meeting-specific: a [`session::Session`] is the root, a
//! pipeline turns its captured media into artifacts (transcript, notes, action items).
//! Meetings are only the first [`session::SessionKind`].

use std::error::Error;
use std::fmt;

/// Error returned when a stored/textual enum value is not recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseEnumError {
    /// Name of the enum that failed to parse.
    pub kind: &'static str,
    /// The offending input.
    pub value: String,
}

impl fmt::Display for ParseEnumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown {} value: {:?}", self.kind, self.value)
    }
}

impl Error for ParseEnumError {}

/// Declares a fieldless enum with a stable textual representation (`as_str` / `FromStr`).
///
/// The text form is what adapters persist, so the literals are part of the storage contract.
macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $text:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant ),+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [$name] = &[ $( $name::$variant ),+ ];

            /// Stable textual form used for persistence.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $( $name::$variant => $text ),+
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::ParseEnumError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $( $text => Ok($name::$variant), )+
                    other => Err($crate::ParseEnumError {
                        kind: stringify!($name),
                        value: other.to_owned(),
                    }),
                }
            }
        }
    };
}

pub mod credentials;
pub mod ids;
pub mod language;
pub mod media;
pub mod note;
pub mod provider;
pub mod session;
pub mod settings;
pub mod transcript;

/// A point in time as milliseconds since the Unix epoch (UTC).
///
/// The domain keeps time as a plain number; calendar handling lives in adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EpochMs(pub i64);

impl EpochMs {
    /// Wraps a raw epoch-millisecond value.
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// The raw epoch-millisecond value.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

pub use credentials::{SecretRef, Variable};
pub use ids::{ActionItemId, FrameId, NoteId, ProviderProfileId, SecretId, SessionId, VariableId};
pub use language::{InvalidLanguageCode, LanguageCode, LanguageSetting, UiLanguage};
pub use media::{AudioTrack, Frame, TrackKind};
pub use note::{ActionItem, Note};
pub use provider::{ProviderKind, ProviderProfile, TaskAssignment, TaskKind};
pub use session::{Session, SessionError, SessionKind, SessionRecord, SessionStatus};
pub use settings::{AppSettings, SettingsError, SettingsPatch};
pub use transcript::TranscriptSegment;

/// Name of the architectural layer this crate implements.
pub const LAYER: &str = "domain";

#[cfg(test)]
mod tests {
    use super::LAYER;

    #[test]
    fn layer_name_is_set() {
        assert_eq!(LAYER, "domain");
    }
}
