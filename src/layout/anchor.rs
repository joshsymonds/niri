//! Tracks the cross-window positioning relationships introduced by
//! `PositionFrame::Window`: which window depends on which target for its
//! anchor, and (in reverse) which dependents each target has.
//!
//! Lives as a generic data structure so it can be tested in pure isolation
//! and so the niri-side `Layout<Mapped>` and the test `Layout<TestWindow>`
//! reuse the same code path. The `Id` type parameter requires `Eq + Hash`
//! (for the HashMap keys) plus `Clone + Debug` (for the LayoutElement::Id
//! bound).
//!
//! The index is purely a bookkeeping data structure — it knows nothing about
//! windows, mapping, or rendering. Layout-level callers feed it identifier
//! pairs as windows map and unmap; later, the geometry-changed handler
//! consults [`AnchorIndex::dependents_of`] to find who needs to be re-placed
//! when a target moves.
//!
//! ## Recursion / cycle policy
//!
//! Per the epic, recursive chains (A → B → C) are *allowed* but logged at
//! `warn!`. They aren't blocked because:
//! - true cycles in user-authored window-rules are config errors the user should learn about, not
//!   silently-rejected behavior
//! - the math code (next task) only ever dereferences one level deep (dependent → target), so
//!   recursive chains don't cause infinite loops at compute time
//!
//! Depth tracking is bounded by `MAX_CHAIN_DEPTH` to keep the registration
//! check itself O(constant).

use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::hash::Hash;

/// Maximum chain depth to walk during recursion detection. Higher than this
/// is almost certainly a true cycle; the cap keeps registration O(1) in
/// pathological cases.
const MAX_CHAIN_DEPTH: usize = 16;

/// Bidirectional index of anchor relationships for cross-window positioning.
///
/// Each entry is a `dependent → target` association. The forward map answers
/// "who is *X* anchored to?" in O(1). The reverse map answers "who is
/// anchored to *X*?" in O(1) — that's the one the geometry-changed handler
/// will hit on the hot path.
#[derive(Debug, Clone)]
pub struct AnchorIndex<Id: Eq + Hash + Clone + Debug> {
    /// dependent → target.
    forward: HashMap<Id, Id>,
    /// target → set of dependents.
    reverse: HashMap<Id, HashSet<Id>>,
}

/// Result of a registration attempt. The caller can use this to surface
/// `warn!` etc.; the registration itself always succeeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterOutcome {
    /// Fresh registration with a target that itself has no target. Chain
    /// depth from the new target is 1.
    Registered,
    /// The target already has a target (chain depth > 1). This is allowed
    /// but the user probably wants to know.
    RecursiveAnchor { chain_depth: usize },
}

impl<Id: Eq + Hash + Clone + Debug> Default for AnchorIndex<Id> {
    fn default() -> Self {
        Self {
            forward: HashMap::new(),
            reverse: HashMap::new(),
        }
    }
}

impl<Id: Eq + Hash + Clone + Debug> AnchorIndex<Id> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `dependent` as anchored to `target`. If `dependent` was
    /// previously registered against a different target, the old registration
    /// is silently replaced (this is the supported re-anchor path, not an
    /// error). Returns whether the new chain is shallow (depth 1) or
    /// recursive (depth > 1, e.g. A → B → C).
    pub fn register(&mut self, dependent: Id, target: Id) -> RegisterOutcome {
        // Walk the forward chain from the new target to measure depth. The
        // existing dependent's old chain is irrelevant because we're about
        // to overwrite it.
        let mut depth = 1usize;
        let mut cursor = self.forward.get(&target);
        while let Some(next) = cursor {
            depth += 1;
            if depth >= MAX_CHAIN_DEPTH {
                break;
            }
            cursor = self.forward.get(next);
        }

        // Drop any prior registration for `dependent` so the maps stay
        // consistent.
        if let Some(old_target) = self.forward.remove(&dependent) {
            if let Some(set) = self.reverse.get_mut(&old_target) {
                set.remove(&dependent);
                if set.is_empty() {
                    self.reverse.remove(&old_target);
                }
            }
        }

        self.forward.insert(dependent.clone(), target.clone());
        self.reverse.entry(target).or_default().insert(dependent);

        if depth > 1 {
            RegisterOutcome::RecursiveAnchor { chain_depth: depth }
        } else {
            RegisterOutcome::Registered
        }
    }

    /// Remove `dependent`'s registration entirely. No-op if it wasn't
    /// registered. Used when the dependent window closes.
    pub fn unregister(&mut self, dependent: &Id) {
        if let Some(target) = self.forward.remove(dependent) {
            if let Some(set) = self.reverse.get_mut(&target) {
                set.remove(dependent);
                if set.is_empty() {
                    self.reverse.remove(&target);
                }
            }
        }
    }

    /// `target` is going away. Returns the dependents that were anchored to
    /// it and clears their forward entries. Callers receive the orphan list
    /// so they can clear any per-dependent state held outside this index
    /// (e.g. last computed position becoming the stored position). The
    /// dependents are NOT re-resolved to new targets — that's the
    /// MRU-at-open-time policy from the epic.
    pub fn orphan_dependents_of(&mut self, target: &Id) -> Vec<Id> {
        let Some(dependents) = self.reverse.remove(target) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(dependents.len());
        for dependent in dependents {
            self.forward.remove(&dependent);
            out.push(dependent);
        }
        out
    }

    /// What is `dependent` anchored to? `None` if free-floating.
    pub fn target_of(&self, dependent: &Id) -> Option<&Id> {
        self.forward.get(dependent)
    }

    /// Who is anchored to `target`?
    pub fn dependents_of(&self, target: &Id) -> impl Iterator<Item = &Id> + '_ {
        self.reverse.get(target).into_iter().flatten()
    }

    /// True if `dependent` has any registered target.
    pub fn is_registered(&self, dependent: &Id) -> bool {
        self.forward.contains_key(dependent)
    }

    /// Iterate every registered dependent (every id with a target). The
    /// reactive re-position trigger uses this in `advance_animations` to
    /// walk only the small set of cross-window-anchored windows rather
    /// than the entire layout.
    pub fn dependents_iter(&self) -> impl Iterator<Item = &Id> + '_ {
        self.forward.keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Idx = AnchorIndex<u32>;

    fn collect_deps(idx: &Idx, target: u32) -> Vec<u32> {
        let mut v: Vec<u32> = idx.dependents_of(&target).copied().collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn register_then_lookup_both_directions() {
        let mut idx = Idx::new();
        let outcome = idx.register(1, 2);
        assert_eq!(outcome, RegisterOutcome::Registered);
        assert_eq!(idx.target_of(&1), Some(&2));
        assert_eq!(collect_deps(&idx, 2), vec![1]);
    }

    #[test]
    fn unregister_clears_both_directions() {
        let mut idx = Idx::new();
        idx.register(1, 2);
        idx.unregister(&1);
        assert_eq!(idx.target_of(&1), None);
        assert!(collect_deps(&idx, 2).is_empty());
        // Inner HashSet should be cleaned up when empty.
        assert!(!idx.reverse.contains_key(&2));
    }

    #[test]
    fn unregister_unknown_dependent_is_noop() {
        let mut idx = Idx::new();
        idx.register(1, 2);
        idx.unregister(&99);
        assert_eq!(idx.target_of(&1), Some(&2));
        assert_eq!(collect_deps(&idx, 2), vec![1]);
    }

    #[test]
    fn orphan_dependents_of_target_returns_them_and_clears() {
        let mut idx = Idx::new();
        idx.register(1, 100);
        idx.register(2, 100);
        idx.register(3, 100);
        let mut orphans = idx.orphan_dependents_of(&100);
        orphans.sort_unstable();
        assert_eq!(orphans, vec![1, 2, 3]);
        // After orphaning, dependents have no target.
        assert_eq!(idx.target_of(&1), None);
        assert_eq!(idx.target_of(&2), None);
        assert_eq!(idx.target_of(&3), None);
        // Reverse entry for 100 is gone.
        assert!(collect_deps(&idx, 100).is_empty());
    }

    #[test]
    fn orphan_unknown_target_returns_empty() {
        let mut idx = Idx::new();
        idx.register(1, 2);
        let orphans = idx.orphan_dependents_of(&999);
        assert!(orphans.is_empty());
        // Unrelated state preserved.
        assert_eq!(idx.target_of(&1), Some(&2));
    }

    #[test]
    fn re_register_replaces_old_target() {
        let mut idx = Idx::new();
        idx.register(1, 2);
        idx.register(1, 3);
        assert_eq!(idx.target_of(&1), Some(&3));
        assert!(collect_deps(&idx, 2).is_empty());
        assert_eq!(collect_deps(&idx, 3), vec![1]);
    }

    #[test]
    fn multiple_dependents_per_target() {
        let mut idx = Idx::new();
        idx.register(1, 100);
        idx.register(2, 100);
        idx.register(3, 100);
        assert_eq!(collect_deps(&idx, 100), vec![1, 2, 3]);
    }

    #[test]
    fn shallow_chain_reports_registered() {
        let mut idx = Idx::new();
        // 1 → 2, where 2 has no target.
        let outcome = idx.register(1, 2);
        assert_eq!(outcome, RegisterOutcome::Registered);
    }

    #[test]
    fn recursive_chain_reports_chain_depth_2() {
        let mut idx = Idx::new();
        // 2 → 3 first.
        idx.register(2, 3);
        // 1 → 2. Chain from 2 is 2 → 3, depth 2.
        let outcome = idx.register(1, 2);
        assert_eq!(outcome, RegisterOutcome::RecursiveAnchor { chain_depth: 2 });
    }

    #[test]
    fn recursive_chain_three_deep_reports_chain_depth_3() {
        let mut idx = Idx::new();
        idx.register(3, 4);
        idx.register(2, 3);
        let outcome = idx.register(1, 2);
        assert_eq!(outcome, RegisterOutcome::RecursiveAnchor { chain_depth: 3 });
    }

    #[test]
    fn pathological_cycle_terminates_at_depth_cap() {
        let mut idx = Idx::new();
        // Build a cycle: 1 → 2, 2 → 1. The second register sees chain
        // 1 → 2 from cursor = forward.get(1) = Some(&2), forward.get(2) =
        // None (we're about to overwrite). Actually we're testing the cap,
        // so let's force a longer chain manually then add the cycle.
        idx.register(1, 2);
        // Manually insert a cycle for the test. We don't have a public
        // method for this; just exercise the path by registering 2 → 1.
        let outcome = idx.register(2, 1);
        // Chain from 1: 1 → 2, but 2's target was about to be overwritten,
        // so during depth-walk we follow 1 → 2 → (forward.get(2) is None at
        // walk time because we haven't inserted yet). So depth = 2.
        // We're not measuring depth perfectly here -- the assertion is just
        // that registration terminates and yields *some* valid outcome.
        assert!(matches!(
            outcome,
            RegisterOutcome::Registered | RegisterOutcome::RecursiveAnchor { .. }
        ));
        // Re-register 1 → 2 again to actually form the cycle, then verify
        // a third registration still terminates.
        let outcome2 = idx.register(1, 2);
        // Now forward map is 1 → 2 and 2 → 1, both directions. Re-walking
        // would loop forever without the cap. Verify it terminates.
        assert!(matches!(outcome2, RegisterOutcome::RecursiveAnchor { .. }));
    }
}
