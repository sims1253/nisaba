//! The shared role vocabulary for Nisaba services.
//!
//! Both the `app` service (REST plane) and the `sync` service (collaboration
//! relay) authorize bearers against the same three roles, and the app's
//! authorization endpoint answers the sync service with the same spellings.
//! This crate owns the parsing side of that vocabulary — the spellings and
//! the parse table both services use for token `roles` claims and sync uses
//! for the endpoint's answers — so consumer-side parsing cannot drift between
//! the services. Emitting the answers (app's `MembershipRole` mapping) stays
//! in the app service.
//!
//! The crate deliberately contains only the vocabulary — [`Role`], its parse
//! table, and its canonical spelling. Authorization *policy* (which role may do
//! what, per-service validation of `iss`/`aud`, per-document grants) stays in
//! each service; `app` owns authorization, `sync` defines only its transport
//! seam over this shared vocabulary.

/// A role a principal can hold. The spellings accepted by [`Role::parse`] are
/// the wire contract: they appear in token `roles` claims and in the app
/// authorization endpoint's responses, so both services must parse them
/// identically — which is why the type lives here rather than in either
/// service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Full read/write.
    Author,
    /// May push updates (reviewers suggest by default); the distinction from
    /// `Author` is enforced by the review/marks layer, not by the role itself.
    Reviewer,
    /// Receives state and presence but cannot mutate the document.
    ReadOnly,
}

impl Role {
    /// Parse a role string as it appears in a token's explicit `roles` claim
    /// or in the app authorization endpoint's response. Unknown spellings are
    /// rejected (`None`) rather than guessed.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "author" => Some(Self::Author),
            "reviewer" => Some(Self::Reviewer),
            "read-only" | "readonly" | "read_only" => Some(Self::ReadOnly),
            _ => None,
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Author => "author",
            Self::Reviewer => "reviewer",
            Self::ReadOnly => "read-only",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_spellings_roundtrip() {
        for role in [Role::Author, Role::Reviewer, Role::ReadOnly] {
            assert_eq!(Role::parse(&role.to_string()), Some(role));
        }
    }

    #[test]
    fn all_read_only_spellings_parse() {
        for spelling in ["read-only", "readonly", "read_only"] {
            assert_eq!(Role::parse(spelling), Some(Role::ReadOnly));
        }
    }

    #[test]
    fn unknown_spellings_are_rejected() {
        assert_eq!(Role::parse("admin"), None);
        assert_eq!(Role::parse(""), None);
        // The table is case-sensitive: tokens spell roles in lower case.
        assert_eq!(Role::parse("Author"), None);
        assert_eq!(Role::parse("READ-ONLY"), None);
    }
}
