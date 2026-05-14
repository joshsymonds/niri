use std::cmp::{max, min};

use niri_config::utils::MergeWith as _;
use niri_config::window_rule::{Match, WindowRule};
use niri_config::{
    BackgroundEffect, BlockOutFrom, BorderRule, CornerRadius, FloatingPosition, PresetSize,
    ResolvedPopupsRules, ShadowRule, TabIndicatorRule,
};
use niri_ipc::ColumnDisplay;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Size};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{
    SurfaceCachedState, ToplevelSurface, XdgToplevelSurfaceRoleAttributes,
};

use crate::utils::with_toplevel_role;

pub mod mapped;
pub use mapped::Mapped;

pub mod unmapped;
pub use unmapped::{InitialConfigureState, Unmapped};

/// Reference to a mapped or unmapped window.
#[derive(Debug, Clone, Copy)]
pub enum WindowRef<'a> {
    Unmapped(&'a Unmapped),
    Mapped(&'a Mapped),
}

/// Rules fully resolved for a window.
#[derive(Debug, Default, PartialEq, Clone)]
pub struct ResolvedWindowRules {
    /// Default width for this window.
    ///
    /// - `None`: unset (global default should be used).
    /// - `Some(None)`: set to empty (window picks its own width).
    /// - `Some(Some(width))`: set to a particular width.
    pub default_width: Option<Option<PresetSize>>,

    /// Default height for this window.
    ///
    /// - `None`: unset (global default should be used).
    /// - `Some(None)`: set to empty (window picks its own height).
    /// - `Some(Some(height))`: set to a particular height.
    pub default_height: Option<Option<PresetSize>>,

    /// Default column display for this window.
    pub default_column_display: Option<ColumnDisplay>,

    /// Default floating position for this window.
    pub default_floating_position: Option<FloatingPosition>,

    /// Output to open this window on.
    pub open_on_output: Option<String>,

    /// Workspace to open this window on.
    pub open_on_workspace: Option<String>,

    /// Whether the window should open full-width.
    pub open_maximized: Option<bool>,

    /// Whether the window should open maximized to edges (true maximized).
    pub open_maximized_to_edges: Option<bool>,

    /// Whether the window should open fullscreen.
    pub open_fullscreen: Option<bool>,

    /// Whether the window should open floating.
    pub open_floating: Option<bool>,

    /// Whether the window should open focused.
    pub open_focused: Option<bool>,

    /// Extra bound on the minimum window width.
    pub min_width: Option<u16>,
    /// Extra bound on the minimum window height.
    pub min_height: Option<u16>,
    /// Extra bound on the maximum window width.
    pub max_width: Option<u16>,
    /// Extra bound on the maximum window height.
    pub max_height: Option<u16>,

    /// Focus ring overrides.
    pub focus_ring: BorderRule,
    /// Window border overrides.
    pub border: BorderRule,
    /// Shadow overrides.
    pub shadow: ShadowRule,
    /// Tab indicator overrides.
    pub tab_indicator: TabIndicatorRule,

    /// Whether or not to draw the border with a solid background.
    ///
    /// `None` means using the SSD heuristic.
    pub draw_border_with_background: Option<bool>,

    /// Extra opacity to draw this window with.
    pub opacity: Option<f32>,

    /// Corner radius to assume this window has.
    pub geometry_corner_radius: Option<CornerRadius>,

    /// Whether to clip this window to its geometry, including the corner radius.
    pub clip_to_geometry: Option<bool>,

    /// Whether to bob this window up and down.
    pub baba_is_float: Option<bool>,

    /// Whether to block out this window from certain render targets.
    pub block_out_from: Option<BlockOutFrom>,

    /// Whether to enable VRR on this window's primary output if it is on-demand.
    pub variable_refresh_rate: Option<bool>,

    /// Multiplier for all scroll events sent to this window.
    pub scroll_factor: Option<f64>,

    /// Override whether to set the Tiled xdg-toplevel state on the window.
    pub tiled_state: Option<bool>,

    /// Background effect configuration.
    pub background_effect: BackgroundEffect,

    /// Rules for this window's popups.
    pub popups: ResolvedPopupsRules,
}

impl<'a> WindowRef<'a> {
    pub fn toplevel(self) -> &'a ToplevelSurface {
        match self {
            WindowRef::Unmapped(unmapped) => unmapped.toplevel(),
            WindowRef::Mapped(mapped) => mapped.toplevel(),
        }
    }

    pub fn is_focused(self) -> bool {
        match self {
            WindowRef::Unmapped(_) => false,
            WindowRef::Mapped(mapped) => mapped.is_focused(),
        }
    }

    pub fn is_urgent(self) -> bool {
        match self {
            WindowRef::Unmapped(_) => false,
            WindowRef::Mapped(mapped) => mapped.is_urgent(),
        }
    }

    pub fn is_active_in_column(self) -> bool {
        match self {
            WindowRef::Unmapped(_) => true,
            WindowRef::Mapped(mapped) => mapped.is_active_in_column(),
        }
    }

    pub fn is_floating(self) -> bool {
        match self {
            // FIXME: This means you cannot set initial configure rules based on is-floating. I'm
            // not sure there's a good way to support it, since this matcher makes a cycle with the
            // open-floating rule.
            //
            // That said, I don't think there are a lot of useful initial configure properties you
            // may want to set through an is-floating matcher? Like, if you're configuring a
            // specific window to open as floating, you can also set those properties in that same
            // window rule, rather than relying on a different is-floating rule.
            WindowRef::Unmapped(_) => false,
            WindowRef::Mapped(mapped) => mapped.is_floating(),
        }
    }

    pub fn is_window_cast_target(self) -> bool {
        match self {
            WindowRef::Unmapped(_) => false,
            WindowRef::Mapped(mapped) => mapped.is_window_cast_target(),
        }
    }
}

impl ResolvedWindowRules {
    pub fn compute(rules: &[WindowRule], window: WindowRef, is_at_startup: bool) -> Self {
        let _span = tracy_client::span!("ResolvedWindowRules::compute");

        let mut resolved = ResolvedWindowRules::default();

        with_toplevel_role(window.toplevel(), |role| {
            // Ensure server_pending like in Smithay's with_pending_state().
            if role.server_pending.is_none() {
                role.server_pending = Some(role.current_server_state().clone());
            }

            let mut open_on_output = None;
            let mut open_on_workspace = None;

            for rule in rules {
                let matches = |m: &Match| {
                    if let Some(at_startup) = m.at_startup {
                        if at_startup != is_at_startup {
                            return false;
                        }
                    }

                    window_matches(window, role, m)
                };

                if !(rule.matches.is_empty() || rule.matches.iter().any(matches)) {
                    continue;
                }

                if rule.excludes.iter().any(matches) {
                    continue;
                }

                if let Some(x) = rule.default_column_width {
                    resolved.default_width = Some(x.0);
                }

                if let Some(x) = rule.default_window_height {
                    resolved.default_height = Some(x.0);
                }

                if let Some(x) = rule.default_column_display {
                    resolved.default_column_display = Some(x);
                }

                if let Some(x) = &rule.default_floating_position {
                    resolved.default_floating_position = Some(x.clone());
                }

                if let Some(x) = rule.open_on_output.as_deref() {
                    open_on_output = Some(x);
                }

                if let Some(x) = rule.open_on_workspace.as_deref() {
                    open_on_workspace = Some(x);
                }

                if let Some(x) = rule.open_maximized {
                    resolved.open_maximized = Some(x);
                }

                if let Some(x) = rule.open_maximized_to_edges {
                    resolved.open_maximized_to_edges = Some(x);
                }

                if let Some(x) = rule.open_fullscreen {
                    resolved.open_fullscreen = Some(x);
                }

                if let Some(x) = rule.open_floating {
                    resolved.open_floating = Some(x);
                }

                if let Some(x) = rule.open_focused {
                    resolved.open_focused = Some(x);
                }

                if let Some(x) = rule.min_width {
                    resolved.min_width = Some(x);
                }
                if let Some(x) = rule.min_height {
                    resolved.min_height = Some(x);
                }
                if let Some(x) = rule.max_width {
                    resolved.max_width = Some(x);
                }
                if let Some(x) = rule.max_height {
                    resolved.max_height = Some(x);
                }

                resolved.focus_ring.merge_with(&rule.focus_ring);
                resolved.border.merge_with(&rule.border);
                resolved.shadow.merge_with(&rule.shadow);
                resolved.tab_indicator.merge_with(&rule.tab_indicator);

                if let Some(x) = rule.draw_border_with_background {
                    resolved.draw_border_with_background = Some(x);
                }
                if let Some(x) = rule.opacity {
                    resolved.opacity = Some(x);
                }
                if let Some(x) = rule.geometry_corner_radius {
                    resolved.geometry_corner_radius = Some(x);
                }
                if let Some(x) = rule.clip_to_geometry {
                    resolved.clip_to_geometry = Some(x);
                }
                if let Some(x) = rule.baba_is_float {
                    resolved.baba_is_float = Some(x);
                }
                if let Some(x) = rule.block_out_from {
                    resolved.block_out_from = Some(x);
                }
                if let Some(x) = rule.variable_refresh_rate {
                    resolved.variable_refresh_rate = Some(x);
                }
                if let Some(x) = rule.scroll_factor {
                    resolved.scroll_factor = Some(x.0);
                }
                if let Some(x) = rule.tiled_state {
                    resolved.tiled_state = Some(x);
                }

                resolved
                    .background_effect
                    .merge_with(&rule.background_effect);

                resolved.popups.merge_with(&rule.popups);
            }

            resolved.open_on_output = open_on_output.map(|x| x.to_owned());
            resolved.open_on_workspace = open_on_workspace.map(|x| x.to_owned());
        });

        resolved
    }

    pub fn apply_min_size(&self, min_size: Size<i32, Logical>) -> Size<i32, Logical> {
        let mut size = min_size;

        if let Some(x) = self.min_width {
            size.w = max(size.w, i32::from(x));
        }
        if let Some(x) = self.min_height {
            size.h = max(size.h, i32::from(x));
        }

        size
    }

    pub fn apply_max_size(&self, max_size: Size<i32, Logical>) -> Size<i32, Logical> {
        let mut size = max_size;

        if let Some(x) = self.max_width {
            if size.w == 0 {
                size.w = i32::from(x);
            } else if x > 0 {
                size.w = min(size.w, i32::from(x));
            }
        }
        if let Some(x) = self.max_height {
            if size.h == 0 {
                size.h = i32::from(x);
            } else if x > 0 {
                size.h = min(size.h, i32::from(x));
            }
        }

        size
    }

    pub fn apply_min_max_size(
        &self,
        min_size: Size<i32, Logical>,
        max_size: Size<i32, Logical>,
    ) -> (Size<i32, Logical>, Size<i32, Logical>) {
        let min_size = self.apply_min_size(min_size);
        let max_size = self.apply_max_size(max_size);
        (min_size, max_size)
    }

    pub fn compute_open_floating(&self, toplevel: &ToplevelSurface) -> bool {
        if let Some(res) = self.open_floating {
            return res;
        }

        // Windows with a parent (usually dialogs) open as floating by default.
        if toplevel.parent().is_some() {
            return true;
        }

        let (min_size, max_size) = with_states(toplevel.wl_surface(), |state| {
            let mut guard = state.cached_state.get::<SurfaceCachedState>();
            let current = guard.current();
            (current.min_size, current.max_size)
        });
        let (min_size, max_size) = self.apply_min_max_size(min_size, max_size);

        // We open fixed-height windows as floating.
        min_size.h > 0 && min_size.h == max_size.h
    }
}

fn window_matches(window: WindowRef, role: &XdgToplevelSurfaceRoleAttributes, m: &Match) -> bool {
    // Must be ensured by the caller.
    let server_pending = role.server_pending.as_ref().unwrap();

    if let Some(is_focused) = m.is_focused {
        if window.is_focused() != is_focused {
            return false;
        }
    }

    if let Some(is_urgent) = m.is_urgent {
        if window.is_urgent() != is_urgent {
            return false;
        }
    }

    if let Some(is_active) = m.is_active {
        // Our "is-active" definition corresponds to the window having a pending Activated state.
        let pending_activated = server_pending
            .states
            .contains(xdg_toplevel::State::Activated);
        if is_active != pending_activated {
            return false;
        }
    }

    if let Some(app_id_re) = &m.app_id {
        let Some(app_id) = &role.app_id else {
            return false;
        };
        if !app_id_re.0.is_match(app_id) {
            return false;
        }
    }

    if let Some(title_re) = &m.title {
        let Some(title) = &role.title else {
            return false;
        };
        if !title_re.0.is_match(title) {
            return false;
        }
    }

    if let Some(is_active_in_column) = m.is_active_in_column {
        if window.is_active_in_column() != is_active_in_column {
            return false;
        }
    }

    if let Some(is_floating) = m.is_floating {
        if window.is_floating() != is_floating {
            return false;
        }
    }

    if let Some(is_window_cast_target) = m.is_window_cast_target {
        if window.is_window_cast_target() != is_window_cast_target {
            return false;
        }
    }

    true
}

/// Adapter that evaluates a [`Match`] against a mapped window, mirroring the
/// shape `ResolvedWindowRules::compute` uses at rule-resolution time. Reuses
/// [`window_matches`] verbatim rather than duplicating the matcher logic.
pub fn mapped_matches(mapped: &Mapped, m: &Match, is_at_startup: bool) -> bool {
    if let Some(at_startup) = m.at_startup {
        if at_startup != is_at_startup {
            return false;
        }
    }
    with_toplevel_role(mapped.toplevel(), |role| {
        if role.server_pending.is_none() {
            role.server_pending = Some(role.current_server_state().clone());
        }
        window_matches(WindowRef::Mapped(mapped), role, m)
    })
}

/// Glue that bridges niri's `Layout<Mapped>` state to the pure [`resolve_target`]
/// helper. Walks all mapped windows, filters by `target` via [`mapped_matches`],
/// excludes the dependent window itself, and projects each survivor into a
/// [`TargetCandidate`]. The result feeds [`resolve_target`] which applies the
/// layered filter (same-workspace → same-output → MRU).
///
/// Returns `Option<Window>` — the smithay `Window` is `<Mapped as
/// LayoutElement>::Id`, which is what `Layout::register_floating_anchor` and
/// the rest of the layout-level anchor API are keyed on.
///
/// This is the only entry point a caller (e.g. dialog-map handler) needs:
/// `Some(target_id)` to anchor, `None` to fall back to working-area positioning.
pub fn resolve_position_frame_target(
    layout: &crate::layout::Layout<Mapped>,
    target: &Match,
    dependent_id: &smithay::desktop::Window,
    dependent_workspace_id: crate::layout::workspace::WorkspaceId,
    dependent_output_name: Option<&str>,
    is_at_startup: bool,
) -> Option<smithay::desktop::Window> {
    let candidates = layout.workspaces().flat_map(|(monitor, _, workspace)| {
        let workspace_id = workspace.id();
        let output_name = monitor.map(|m| m.output_name().clone());
        workspace.windows().filter_map(move |mapped| {
            // Use the LayoutElement::Id (the smithay Window) for both the
            // dependent-skip check and the resulting TargetCandidate so the
            // key type matches the anchor index.
            let window_id = <Mapped as crate::layout::LayoutElement>::id(mapped).clone();
            if &window_id == dependent_id {
                return None;
            }
            if !mapped_matches(mapped, target, is_at_startup) {
                return None;
            }
            Some(TargetCandidate {
                id: window_id,
                workspace_id,
                output_name: output_name.clone(),
                focus_timestamp: mapped.get_focus_timestamp(),
            })
        })
    });
    resolve_target(candidates, dependent_workspace_id, dependent_output_name)
}

/// A minimal projection of a mapped window's state for the cross-window
/// positioning target resolver. Constructed by [`resolve_position_frame_target`]
/// when iterating mapped windows; consumed by [`resolve_target`].
///
/// Generic on the id type so the pure resolver is testable with simple values
/// (e.g. `u32`) while production `Layout<Mapped>` carries `Window` (smithay's
/// `LayoutElement::Id` for `Mapped`).
///
/// Owning `output_name` (rather than borrowing) keeps the resolver decoupled
/// from `Layout`'s lifetimes — the resolver is a pure function that can be
/// tested without any layout fixture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetCandidate<Id> {
    pub id: Id,
    pub workspace_id: crate::layout::workspace::WorkspaceId,
    /// `None` for the `NoOutputs` monitor-set case (workspaces with no
    /// associated output).
    pub output_name: Option<String>,
    /// `None` if the window has never been focused. Resolved by [`resolve_target`]
    /// as strictly less recent than any `Some(_)`.
    pub focus_timestamp: Option<std::time::Duration>,
}

/// Pick a target window from a set of `Match`-matched candidates, applying the
/// epic's layered filter: same-workspace beats same-output beats anywhere, and
/// within each layer the most-recently-focused candidate wins. Returns the
/// chosen window's `MappedId` or `None` if `candidates` was empty.
///
/// The caller is responsible for filtering out the *dependent* window itself
/// before calling — this function will happily return the dependent's own id
/// if it's present.
///
/// Ordering of ties: when multiple candidates compare equal under the MRU
/// rule (including the "all `None` timestamps" case), iteration order wins
/// (first-seen is kept).
pub fn resolve_target<Id, I>(
    candidates: I,
    dependent_workspace_id: crate::layout::workspace::WorkspaceId,
    dependent_output_name: Option<&str>,
) -> Option<Id>
where
    Id: Clone,
    I: IntoIterator<Item = TargetCandidate<Id>>,
{
    let all: Vec<TargetCandidate<Id>> = candidates.into_iter().collect();
    if all.is_empty() {
        return None;
    }

    // Layer 1: same workspace.
    let by_workspace: Vec<TargetCandidate<Id>> = all
        .iter()
        .filter(|c| c.workspace_id == dependent_workspace_id)
        .cloned()
        .collect();
    if !by_workspace.is_empty() {
        return pick_mru(by_workspace);
    }

    // Layer 2: same output.
    let by_output: Vec<TargetCandidate<Id>> = all
        .iter()
        .filter(|c| c.output_name.as_deref() == dependent_output_name)
        .cloned()
        .collect();
    if !by_output.is_empty() {
        return pick_mru(by_output);
    }

    // Layer 3: anywhere.
    pick_mru(all)
}

fn pick_mru<Id, I>(it: I) -> Option<Id>
where
    I: IntoIterator<Item = TargetCandidate<Id>>,
{
    let mut best: Option<TargetCandidate<Id>> = None;
    for c in it {
        let beats = match best.as_ref() {
            None => true,
            Some(b) => match (c.focus_timestamp, b.focus_timestamp) {
                (Some(ct), Some(bt)) => ct > bt,
                (Some(_), None) => true,
                (None, _) => false,
            },
        };
        if beats {
            best = Some(c);
        }
    }
    best.map(|c| c.id)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::layout::workspace::WorkspaceId;

    /// Source of unique ids for tests; each `cand` call burns one. The id
    /// type is just `u32` — the resolver is generic, so the production
    /// `Window` type isn't needed for testing.
    static NEXT_TEST_ID: AtomicU32 = AtomicU32::new(1);

    fn cand(ws: u64, output: Option<&str>, ts_micros: Option<u64>) -> TargetCandidate<u32> {
        TargetCandidate {
            id: NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed),
            workspace_id: WorkspaceId::specific(ws),
            output_name: output.map(str::to_owned),
            focus_timestamp: ts_micros.map(Duration::from_micros),
        }
    }

    #[test]
    fn resolve_target_no_candidates_returns_none() {
        let result = resolve_target(
            Vec::<TargetCandidate<u32>>::new(),
            WorkspaceId::specific(1),
            Some("DP-1"),
        );
        assert_eq!(result, None);
    }

    #[test]
    fn resolve_target_single_candidate_returns_it_regardless_of_match() {
        let c = cand(99, Some("DP-99"), Some(10));
        let id = c.id;
        let result = resolve_target(vec![c], WorkspaceId::specific(1), Some("DP-1"));
        assert_eq!(result, Some(id));
    }

    #[test]
    fn resolve_target_prefers_same_workspace_over_other_workspace_even_with_older_ts() {
        // Older timestamp but right workspace.
        let on_ws = cand(1, Some("DP-1"), Some(1));
        let on_ws_id = on_ws.id;
        // Newer timestamp but wrong workspace.
        let off_ws = cand(2, Some("DP-1"), Some(1000));
        let result = resolve_target(vec![off_ws, on_ws], WorkspaceId::specific(1), Some("DP-1"));
        assert_eq!(result, Some(on_ws_id));
    }

    #[test]
    fn resolve_target_prefers_same_output_when_no_workspace_match() {
        // No candidate on workspace 1; we should prefer same output.
        let on_output = cand(5, Some("DP-1"), Some(1));
        let on_output_id = on_output.id;
        let off_output = cand(7, Some("DP-2"), Some(1000));
        let result = resolve_target(
            vec![off_output, on_output],
            WorkspaceId::specific(1),
            Some("DP-1"),
        );
        assert_eq!(result, Some(on_output_id));
    }

    #[test]
    fn resolve_target_falls_through_to_all_when_neither_workspace_nor_output_match() {
        let only = cand(99, Some("DP-99"), Some(42));
        let only_id = only.id;
        let result = resolve_target(vec![only], WorkspaceId::specific(1), Some("DP-1"));
        assert_eq!(result, Some(only_id));
    }

    #[test]
    fn resolve_target_picks_mru_within_same_workspace_set() {
        let older = cand(1, Some("DP-1"), Some(100));
        let newer = cand(1, Some("DP-1"), Some(200));
        let newer_id = newer.id;
        let result = resolve_target(vec![older, newer], WorkspaceId::specific(1), Some("DP-1"));
        assert_eq!(result, Some(newer_id));
    }

    #[test]
    fn resolve_target_some_timestamp_beats_none_timestamp() {
        let no_ts = cand(1, Some("DP-1"), None);
        let with_ts = cand(1, Some("DP-1"), Some(1));
        let with_ts_id = with_ts.id;
        let result = resolve_target(vec![no_ts, with_ts], WorkspaceId::specific(1), Some("DP-1"));
        assert_eq!(result, Some(with_ts_id));
    }

    #[test]
    fn resolve_target_all_none_timestamps_picks_first_in_iteration_order() {
        let first = cand(1, Some("DP-1"), None);
        let first_id = first.id;
        let second = cand(1, Some("DP-1"), None);
        let result = resolve_target(vec![first, second], WorkspaceId::specific(1), Some("DP-1"));
        assert_eq!(result, Some(first_id));
    }

    #[test]
    fn resolve_target_short_circuits_to_workspace_ignoring_better_match_elsewhere() {
        // Workspace match is the WORST candidate everywhere else,
        // but it's the right workspace so it must win.
        let bad_on_ws = cand(1, None, None);
        let bad_on_ws_id = bad_on_ws.id;
        let good_off_ws = cand(2, Some("DP-1"), Some(9999));
        let result = resolve_target(
            vec![good_off_ws, bad_on_ws],
            WorkspaceId::specific(1),
            Some("DP-1"),
        );
        assert_eq!(result, Some(bad_on_ws_id));
    }

    #[test]
    fn resolve_target_handles_no_output_dependent() {
        // Dependent has no output (NoOutputs case); output filter should match
        // candidates whose output_name is also None.
        let on_no_output = cand(7, None, Some(1));
        let on_no_output_id = on_no_output.id;
        let off_output = cand(8, Some("DP-1"), Some(1000));
        let result = resolve_target(
            vec![off_output, on_no_output],
            WorkspaceId::specific(1),
            None,
        );
        assert_eq!(result, Some(on_no_output_id));
    }
}
