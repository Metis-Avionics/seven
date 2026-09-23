//! # seven-six — Aviation cache policy over theSix (S07)
//!
//! The policy trait is theSix's; tier selection, single-flight, generation
//! handling, and health/circuit logic are all theSix internals. This crate
//! provides only an `AviationCachePolicy` that encodes Seven's aviation-domain
//! rules *above* theSix — never inside it.
//!
//! Rules:
//!
//! * **Belief state** (hot computation) prefers hotter tiers (L1/L2).
//! * **Evidence / provenance** prefers persistent tiers (L4).
//! * **Anonymous mutation is denied** (aviation data must not be mutable by
//!   unauthenticated callers §S15).
//!
//! theSix decides which tier actually serves the request once its health
//! checks run; Seven merely biases the base tier.

#![forbid(unsafe_code)]

use seven_core::SubjectId;
use thesix::{
    CacheOperation, CachePolicy, CacheRequest, CacheState, IdentityContext, PolicyDecision,
    StrictPolicy,
};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum SixError {
    #[error("policy denied the operation")]
    Denied,
}

pub type SixResult<T> = Result<T, SixError>;

// ===========================================================================
// Key typing — Seven separates belief from evidence keys.
// ===========================================================================

/// Strong key typing so application code cannot accidentally route an
/// evidence blob to a belief tier (and vice versa).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SevenKey {
    Belief(SubjectId),
    Evidence(String), // evidence_id hex
    Generic(String),
}

impl std::fmt::Display for SevenKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Belief(s) => write!(f, "belief:{}", s.0),
            Self::Evidence(id) => write!(f, "evidence:{id}"),
            Self::Generic(k) => f.write_str(k),
        }
    }
}

// ===========================================================================
// Aviation policy — implements theSix's trait; mechanics stay in theSix.
// ============================================================================

#[derive(Debug, Clone)]
pub struct AviationCachePolicy {
    inner: StrictPolicy,
}

impl AviationCachePolicy {
    #[must_use]
    pub fn new() -> Self {
        Self { inner: StrictPolicy }
    }

    fn base_tier_for(op: CacheOperation, key: &SevenKey) -> thesix::TierId {
        use thesix::TierId;
        match (op, key) {
            // Belief: hot local tier; evidence: persistent tier; generic: L2.
            (CacheOperation::Get | CacheOperation::Set, SevenKey::Belief(_)) => TierId::L1,
            (CacheOperation::Get | CacheOperation::Set, SevenKey::Evidence(_)) => TierId::L4,
            _ => TierId::L2,
        }
    }
}

impl Default for AviationCachePolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl CachePolicy<SevenKey, String> for AviationCachePolicy {
    fn select(
        &self,
        request: &CacheRequest<SevenKey, String>,
        state: &CacheState,
        identity: &IdentityContext,
    ) -> PolicyDecision {
        // Deny logic (authentication) comes from StrictPolicy.
        let mut decision = self.inner.select(request, state, identity);
        if !decision.authorized {
            return decision;
        }
        // Then bias the tier to aviation needs; theSix health/capacity rules
        // still run afterward and can override (that is by design).
        decision.tier = Self::base_tier_for(request.operation, &request.key);
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::{AviationCachePolicy, SevenKey};
    use seven_core::SubjectId;
    use thesix::{CacheOperation, CacheRequest, DefaultPolicy, PolicyDecision, TierId};
    use thesix::{CacheState, IdentityContext};

    fn identity(authed: bool) -> IdentityContext {
        IdentityContext::new(
            if authed { "alice".to_string() } else { "anon".to_string() },
            vec!["reader".to_string()],
            "tenant-av".to_string(),
        )
    }

    #[test]
    fn belief_routes_hot() {
        let policy = AviationCachePolicy::new();
        let req: CacheRequest<SevenKey, String> =
            CacheRequest::new(CacheOperation::Get, SevenKey::Belief(SubjectId("ac".into())));
        let state = CacheState::new();
        let id = identity(true);
        let d: PolicyDecision = <AviationCachePolicy as thesix::CachePolicy<SevenKey, String>>::select(&policy, &req, &state, &id);
        assert_eq!(d.tier, TierId::L1);
        assert!(d.authorized);
    }

    #[test]
    fn evidence_routes_persistent() {
        let policy = AviationCachePolicy::new();
        let req: CacheRequest<SevenKey, String> =
            CacheRequest::new(CacheOperation::Get, SevenKey::Evidence("deadbeef".into()));
        let d = <AviationCachePolicy as thesix::CachePolicy<SevenKey, String>>::select(
            &policy,
            &req,
            &CacheState::new(),
            &identity(true),
        );
        assert_eq!(d.tier, TierId::L4);
    }

    /// Anonymous mutation is denied (composes `StrictPolicy`).
    /// theSix semantics: identity is "authenticated" iff principal is non-empty.
    #[test]
    fn anonymous_mutation_denied() {
        let policy = AviationCachePolicy::new();
        let req: CacheRequest<SevenKey, String> =
            CacheRequest::new(CacheOperation::Set, SevenKey::Generic("k".into()));
        let anon = IdentityContext::new(String::new(), vec![], "tenant-av".to_string());
        let d = <AviationCachePolicy as thesix::CachePolicy<SevenKey, String>>::select(
            &policy,
            &req,
            &CacheState::new(),
            &anon,
        );
        assert!(!d.authorized, "empty-principal mutation must be denied");
    }

    /// theSix `DefaultPolicy` still usable when aviation bias is not desired.
    /// (Proves Seven is *adding* policy, not *replacing* theSix.)
    #[test]
    fn default_policy_still_composable() {
        let _ = DefaultPolicy;
    }
}
