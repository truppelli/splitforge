//! Identifier newtypes.
//!
//! Evidence identifiers (`RawReadId`) are random v4 UUIDs minted once, at ingest.
//! Derived identifiers (`AcceptedReadId`, `TimingEventId`) are **deterministic** v5 UUIDs
//! computed from the evidence that produced them — see [`crate::derived`]. That is what
//! makes re-derivation bit-identical rather than merely equivalent.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// UUID namespace for every SplitForge-derived identifier.
///
/// Generated once, hardcoded forever. Changing it would silently change every derived id
/// in existing databases.
pub const SPLITFORGE_NAMESPACE: Uuid = Uuid::from_u128(0x5f17_f03e_4b2a_4c81_9d6e_7a13_c5b8_0921);

macro_rules! uuid_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(
            /// The underlying UUID.
            pub Uuid,
        );

        impl $name {
            /// Mints a new random identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Wraps an existing UUID.
            #[must_use]
            pub const fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            /// Returns the underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }
    };
}

macro_rules! string_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(
            /// The underlying string.
            pub String,
        );

        impl $name {
            /// Wraps an existing string.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the value as a string slice.
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

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

uuid_newtype! {
    /// Identifies a single raw read. Minted at ingest and never reused.
    RawReadId
}

uuid_newtype! {
    /// Identifies an accepted read. Deterministic — see [`crate::derived::AcceptedRead`].
    AcceptedReadId
}

uuid_newtype! {
    /// Identifies a timing event. Deterministic — see [`crate::derived::TimingEvent`].
    TimingEventId
}

uuid_newtype! {
    /// Identifies a manual entry.
    ///
    /// Random v4, not derived: an operator typing a bib and a time is an **act**, and two
    /// operators recording the same runner at the same second are two separate records of
    /// it, not one. Deriving the identifier from the contents would silently collapse them.
    ManualEntryId
}

uuid_newtype! {
    /// Identifies a participant.
    ParticipantId
}

uuid_newtype! {
    /// Identifies a published result revision.
    ///
    /// Random v4, not derived: publishing is an **act**, not a conclusion. Two revisions can
    /// hold identical numbers and still be distinct events in the race's history — the
    /// second one exists because somebody pressed publish again, and that fact is the
    /// record. See `docs/timing-model.md` § 7.
    ResultRevisionId
}

uuid_newtype! {
    /// Identifies an operator's status declaration. Minted once, never reused: a declaration
    /// is evidence that somebody said something.
    StatusDeclarationId
}

uuid_newtype! {
    /// Identifies an event (a race day, which may contain several races).
    EventId
}

uuid_newtype! {
    /// Identifies a race within an event.
    RaceId
}

uuid_newtype! {
    /// Identifies a checkpoint on a course.
    CheckpointId
}

string_newtype! {
    /// Identifies a reader.
    ///
    /// Deliberately operator-assigned and human-meaningful (`"finish-line"`), not derived
    /// from a DHCP lease or MAC address — a reader that gets a new IP is still the same
    /// reader, and evidence must not become ambiguous because the network changed.
    ReaderId
}

string_newtype! {
    /// A chip identifier, normally an EPC in uppercase hex.
    ChipId
}

string_newtype! {
    /// A competitor's bib number. A string, because bibs are not always numeric.
    Bib
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_ids_round_trip_through_strings() {
        let id = RawReadId::new();
        let parsed: RawReadId = id.to_string().parse().expect("round trip");
        assert_eq!(id, parsed);
    }

    #[test]
    fn uuid_ids_are_unique() {
        assert_ne!(RawReadId::new(), RawReadId::new());
    }

    #[test]
    fn string_ids_round_trip_through_serde() {
        let chip = ChipId::new("E280116060000204FCB8D3A1");
        let json = serde_json::to_string(&chip).expect("serialize");
        assert_eq!(json, "\"E280116060000204FCB8D3A1\"");
        let back: ChipId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(chip, back);
    }
}
