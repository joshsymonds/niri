use std::collections::HashSet;

use crate::niri::CastTarget;

/// Derived view of [`Screencasting::casts`]: which outputs and which window-ids
/// are currently being cast. Recomputed from scratch whenever the live cast set
/// changes (start, target switch, stop). Consumers (indicator border, layer
/// hiding, Zoom auto-hide) read from this single source of truth instead of
/// scanning `casts` themselves.
#[derive(Debug, Default, Clone)]
pub struct ActiveCasts {
    pub outputs: HashSet<String>,
    pub windows: HashSet<u64>,
}

impl ActiveCasts {
    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty() && self.windows.is_empty()
    }

    pub fn contains_output(&self, name: &str) -> bool {
        self.outputs.contains(name)
    }

    pub fn contains_window(&self, id: u64) -> bool {
        self.windows.contains(&id)
    }

    /// Replace the current snapshot with one derived from `targets`. Each
    /// non-`Nothing` target contributes either its output name or its window id
    /// to the corresponding set. Duplicate targets coalesce (HashSet).
    pub fn recompute_from_targets<'a, I>(&mut self, targets: I)
    where
        I: IntoIterator<Item = &'a CastTarget>,
    {
        self.outputs.clear();
        self.windows.clear();
        for target in targets {
            match target {
                CastTarget::Nothing => {}
                CastTarget::Output { name, .. } => {
                    self.outputs.insert(name.clone());
                }
                CastTarget::Window { id } => {
                    self.windows.insert(*id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use smithay::output::{Output, PhysicalProperties, Subpixel};

    use super::*;

    fn mock_output(name: &str) -> Output {
        Output::new(
            name.to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".to_string(),
                model: "test".to_string(),
                serial_number: "0".to_string(),
            },
        )
    }

    #[test]
    fn empty_by_default() {
        let active = ActiveCasts::default();
        assert!(active.is_empty());
        assert!(!active.contains_output("anything"));
        assert!(!active.contains_window(0));
    }

    #[test]
    fn recompute_inserts_window_targets() {
        let mut active = ActiveCasts::default();
        let targets = [CastTarget::Window { id: 42 }, CastTarget::Window { id: 7 }];
        active.recompute_from_targets(&targets);

        assert!(active.contains_window(42));
        assert!(active.contains_window(7));
        assert!(!active.contains_window(99));
        assert!(!active.is_empty());
    }

    #[test]
    fn recompute_inserts_output_targets() {
        let out_a = mock_output("HDMI-A-1");
        let out_b = mock_output("DP-2");
        let targets = [CastTarget::output(&out_a), CastTarget::output(&out_b)];

        let mut active = ActiveCasts::default();
        active.recompute_from_targets(&targets);

        assert!(active.contains_output("HDMI-A-1"));
        assert!(active.contains_output("DP-2"));
        assert!(!active.contains_output("eDP-1"));
    }

    #[test]
    fn recompute_ignores_nothing_targets() {
        let mut active = ActiveCasts::default();
        let targets = [CastTarget::Nothing, CastTarget::Nothing];
        active.recompute_from_targets(&targets);

        assert!(active.is_empty());
    }

    #[test]
    fn recompute_clears_stale_entries() {
        let mut active = ActiveCasts::default();

        let targets = [CastTarget::Window { id: 1 }];
        active.recompute_from_targets(&targets);
        assert!(active.contains_window(1));

        // New snapshot replaces the old one entirely.
        let targets = [CastTarget::Window { id: 2 }];
        active.recompute_from_targets(&targets);

        assert!(!active.contains_window(1));
        assert!(active.contains_window(2));
    }

    #[test]
    fn recompute_dedupes_repeated_targets() {
        let mut active = ActiveCasts::default();
        let targets = [
            CastTarget::Window { id: 5 },
            CastTarget::Window { id: 5 },
            CastTarget::Window { id: 5 },
        ];
        active.recompute_from_targets(&targets);

        assert!(active.contains_window(5));
        assert_eq!(active.windows.len(), 1);
    }

    #[test]
    fn recompute_distinguishes_output_and_window() {
        let out = mock_output("HDMI-A-1");
        let targets = [CastTarget::output(&out), CastTarget::Window { id: 1 }];

        let mut active = ActiveCasts::default();
        active.recompute_from_targets(&targets);

        assert!(active.contains_output("HDMI-A-1"));
        assert!(!active.contains_output("1"));
        assert!(active.contains_window(1));
        assert!(!active.contains_window(0));
    }

    #[test]
    fn recompute_with_no_targets_is_empty() {
        let mut active = ActiveCasts::default();
        active.recompute_from_targets(std::iter::empty::<&CastTarget>());
        assert!(active.is_empty());
    }
}
