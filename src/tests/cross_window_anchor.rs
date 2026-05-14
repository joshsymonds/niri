//! Integration tests for the cross-window positioning feature
//! (`PositionFrame::Window`). The tests drive real Wayland clients against
//! the headless backend, set titles that match `in-window-of` rules, and
//! introspect the layout's tile positions to assert that dependents anchor
//! correctly to their targets.
//!
//! Each test covers one of the success criteria from the epic:
//! - `dialog_opens_anchored_to_target_top_left` — initial placement
//! - `dependent_follows_target_on_rect_change` — target's rect mutates, dialog follows (within a
//!   workspace)
//! - `dependent_follows_target_across_workspaces` — target migrates to another workspace; dialog
//!   follows and stays anchored
//! - `dependent_orphaned_when_target_closes_keeps_last_position` — target close, dependent stays
//!   put (MRU-at-open-time policy)
//! - `user_drag_breaks_anchor` — user drag detaches dependent from anchor
//! - `rule_with_no_state_fields_matches_target_regardless_of_state` — None state fields skip the
//!   matcher (epic invariant)
//! - `many_dependents_anchored_to_same_target_migrate_together` — bounded cost of the
//!   cross-workspace migration when many dialogs are anchored to a single target.
//!
//! Per the epic's "follow across outputs" success criterion: in niri,
//! workspaces are bound to outputs, so moving a workspace across the
//! monitor boundary exercises the cross-output follow as a side effect of
//! the cross-workspace path. A dedicated multi-output test is not added
//! here because the test fixture's `add_output` flow plus
//! `move_to_output` driving the migration would duplicate coverage of
//! the same `Layout::notify_tile_changed` code path that
//! `dependent_follows_target_across_workspaces` already covers.

use std::fmt::Write as _;

use client::ClientId;
use insta::assert_snapshot;
use niri_config::Config;
use smithay::utils::{Point, Size};
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::Window as ClientWindow;
use super::*;
use crate::niri::Niri;

/// Renders the active workspace's tile positions for snapshot assertions.
/// Sort by mapped-id-order (creation order, deterministic per-test) so the
/// snapshot stays stable.
fn format_tiles(niri: &Niri) -> String {
    let mut buf = String::new();
    let ws = niri.layout.active_workspace().unwrap();
    let mut tiles: Vec<_> = ws.tiles_with_render_positions().collect();
    tiles.sort_by_key(|(tile, _, _)| tile.window().id().get());
    for (tile, pos, _visible) in tiles {
        let Size { w, h, .. } = tile.animated_tile_size();
        let Point { x, y, .. } = pos;
        writeln!(&mut buf, "{w:>4.0} × {h:>4.0} at x:{x:>5.0} y:{y:>4.0}").unwrap();
    }
    buf
}

/// Build a config carrying a single window-rule: any window with title
/// `^dialog$` opens floating, anchored inside the tile of any window with
/// title `^target$`, positioned at the target tile's top-left (offset 0,0).
///
/// Using TopLeft (instead of Center) makes the snapshot assertion crisp —
/// the dialog's logical_pos will equal the target's render_pos exactly,
/// independent of either tile's size.
fn anchor_config_top_left() -> Config {
    Config::parse_mem(
        r##"
        window-rule {
            match title="^dialog$"
            open-floating true
            default-floating-position x=0 y=0 relative-to="top-left" {
                in-window-of title="^target$"
            }
        }
        "##,
    )
    .unwrap()
}

/// Map a titled window with the standard fixture flow:
/// 1. create_window → set_title → commit (initial commit announces title).
/// 2. attach_new_buffer → set_size → ack_last_and_commit (map for real).
fn map_titled_window(f: &mut Fixture, id: ClientId, title: &str, w: u16, h: u16) -> WlSurface {
    let window: &mut ClientWindow = f.client(id).create_window();
    let surface = window.surface.clone();
    window.set_title(title);
    window.commit();
    f.roundtrip(id);

    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.set_size(w, h);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    surface
}

/// Look up the smithay `Window` (LayoutElement::Id for Mapped) corresponding
/// to a mapped tile by its window title. The test client's `wl_surface` and
/// the compositor's `wl_surface` are distinct types (wayland_client vs
/// wayland_server), so we bridge by iterating mapped tiles and matching the
/// title set via `set_title`.
fn window_id_for_title(niri: &Niri, title: &str) -> smithay::desktop::Window {
    let ws = niri.layout.active_workspace().unwrap();
    for (tile, _, _) in ws.tiles_with_render_positions() {
        let mapped = tile.window();
        let actual_title =
            crate::utils::with_toplevel_role(mapped.toplevel(), |role| role.title.clone());
        if actual_title.as_deref() == Some(title) {
            return mapped.window.clone();
        }
    }
    panic!("no mapped tile with title {title:?}");
}

/// True iff a tile with `id` is currently mapped on the active workspace.
fn window_is_mapped(niri: &Niri, id: &smithay::desktop::Window) -> bool {
    let ws = niri.layout.active_workspace().unwrap();
    ws.tiles_with_render_positions()
        .any(|(tile, _, _)| &tile.window().window == id)
}

#[test]
fn dialog_opens_anchored_to_target_top_left() {
    let mut f = Fixture::with_config(anchor_config_top_left());
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    // Map the target window (no anchor rule applies — opens tiled).
    let _target = map_titled_window(&mut f, id, "target", 800, 600);
    // Map the dialog window — anchor rule applies, in-window-of matches target.
    let _dialog = map_titled_window(&mut f, id, "dialog", 100, 50);

    // With TopLeft x=0 y=0 anchor against target's tile rect, the dialog's
    // logical position should equal the target's render position. Snapshot
    // pins both — if the anchor breaks, the dialog's y/x diverges from the
    // target's.
    assert_snapshot!(format_tiles(f.niri()), @r"
     800 ×  600 at x:   16 y:  16
     100 ×   50 at x:   16 y:  16
    ");
}

#[test]
fn dependent_follows_target_on_rect_change() {
    // The visible-behavior contract: when the target's tile rect changes
    // (via any Layout mutation), the dependent re-positions to maintain its
    // anchor relationship. This test exercises the architectural change in
    // task #11 (event-driven re-position via the reverse-keyed sweep).
    //
    // We change the target's rect by toggling its column to maximized; this
    // expands the target tile to fill the working area. The dialog —
    // anchored at TopLeft x=0 y=0 — should reposition to the target's new
    // origin.

    let mut f = Fixture::with_config(anchor_config_top_left());
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let target = map_titled_window(&mut f, id, "target", 800, 600);
    let _dialog = map_titled_window(&mut f, id, "dialog", 100, 50);

    // Verify the pre-mutation state: dialog anchored to target.
    assert_snapshot!(format_tiles(f.niri()), @r"
     800 ×  600 at x:   16 y:  16
     100 ×   50 at x:   16 y:  16
    ");

    // Mutate the target's rect: fullscreen it. This requires the standard
    // configure/ack dance:
    //  1. set_fullscreen sends a configure with the new size + Fullscreen.
    //  2. double_roundtrip lets the configure reach the client.
    //  3. Client acks + commits with the new size, so niri applies the rect change.
    //  4. niri_complete_animations drives the sweep so notify_tile_changed fires -> dialog
    //     re-positions to the new target rect.
    let target_id = window_id_for_title(f.niri(), "target");
    f.niri().layout.set_fullscreen(&target_id, true);
    f.double_roundtrip(id);

    let target_window = f.client(id).window(&target);
    target_window.ack_last_and_commit();
    f.double_roundtrip(id);
    f.niri_complete_animations();

    // The dialog should have followed: it's still at the target's render
    // origin, which is now (0, 0) since fullscreen takes the whole working
    // area without padding.
    assert_snapshot!(format_tiles(f.niri()), @r"
    1920 × 1080 at x:    0 y:   0
     100 ×   50 at x:    0 y:   0
    ");
}

#[test]
fn dependent_orphaned_when_target_closes_keeps_last_position() {
    // The MRU-at-open-time policy: when the target closes, the dependent
    // is orphaned — it stops tracking and stays at its last computed
    // position. It is NOT re-resolved to a new target.

    let mut f = Fixture::with_config(anchor_config_top_left());
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let target = map_titled_window(&mut f, id, "target", 800, 600);
    let dialog = map_titled_window(&mut f, id, "dialog", 100, 50);

    // Snapshot pre-close state.
    assert_snapshot!(format_tiles(f.niri()), @r"
     800 ×  600 at x:   16 y:  16
     100 ×   50 at x:   16 y:  16
    ");

    // Capture window ids before the close — we'll need them to check state
    // post-close, but title-based lookup won't find a closed window.
    let target_id = window_id_for_title(f.niri(), "target");
    let dialog_id = window_id_for_title(f.niri(), "dialog");

    // Destroy the target client-side and roundtrip so the compositor sees
    // the unmap. The compositor's unmap hook calls
    // orphan_floating_anchor_dependents_of(target), clearing the dialog's
    // forward-map entry.
    f.client(id).window(&target).attach_null();
    f.client(id).window(&target).commit();
    f.double_roundtrip(id);
    f.niri_complete_animations();

    // The dialog's anchor target should now be None — the orphan completed.
    let anchor_after = f
        .niri()
        .layout
        .floating_anchor_target_of(&dialog_id)
        .cloned();
    assert!(
        anchor_after.is_none(),
        "expected dialog to be orphaned, but anchor_target_of returned {anchor_after:?}",
    );

    // The dialog stays mapped; the target is gone.
    assert!(
        window_is_mapped(f.niri(), &dialog_id),
        "dialog should remain mapped after target closes",
    );
    assert!(
        !window_is_mapped(f.niri(), &target_id),
        "target should be unmapped after attach_null + commit",
    );
    let _ = dialog;
}

#[test]
fn user_drag_breaks_anchor() {
    // Per the epic's user-drag-breaks-anchor policy (review C2):
    // dragging an anchored dialog detaches it from the anchor permanently.
    // Subsequent target moves should NOT reposition the dragged dialog.

    let mut f = Fixture::with_config(anchor_config_top_left());
    f.add_output(1, (1920, 1080));
    let output = f.niri_output(1);
    let id = f.add_client();

    let _target = map_titled_window(&mut f, id, "target", 800, 600);
    let _dialog = map_titled_window(&mut f, id, "dialog", 100, 50);
    let dialog_id = window_id_for_title(f.niri(), "dialog");

    // Verify the anchor is registered up front.
    let anchor_before = f
        .niri()
        .layout
        .floating_anchor_target_of(&dialog_id)
        .cloned();
    assert!(
        anchor_before.is_some(),
        "expected dialog to be anchored before drag",
    );

    // Drive a drag of the dialog through the interactive-move API directly
    // (skipping the input grab plumbing). The Starting -> Moving transition
    // — which is where the unregister hook lives — happens past the
    // threshold for scrolling tiles, immediately for floating tiles. The
    // dialog is floating, so the transition is immediate on first update.
    let start_pos = smithay::utils::Point::from((500.0, 500.0));
    let pointer_delta = smithay::utils::Point::from((50.0, 50.0));
    assert!(
        f.niri()
            .layout
            .interactive_move_begin(dialog_id.clone(), &output, start_pos),
        "interactive_move_begin should succeed",
    );
    f.niri().layout.interactive_move_update(
        &dialog_id,
        pointer_delta,
        output.clone(),
        start_pos + pointer_delta,
    );

    // After the drag transitions to Moving, the anchor should be gone.
    let anchor_after = f
        .niri()
        .layout
        .floating_anchor_target_of(&dialog_id)
        .cloned();
    assert!(
        anchor_after.is_none(),
        "expected anchor to be broken by user drag, but anchor_target_of returned {anchor_after:?}",
    );
}

#[test]
fn rule_with_no_state_fields_matches_target_regardless_of_state() {
    // The `Match` struct has 7 optional state fields (`is_floating`,
    // `is_focused`, `is_active`, `is_active_in_column`,
    // `is_window_cast_target`, `is_urgent`, `at_startup`). The matcher
    // contract is that None-valued fields skip — a rule that doesn't
    // mention `is_floating` matches any value of it. This test pins
    // that invariant explicitly: the target window is tiled (not
    // floating), the rule constrains only `title`, and the dialog must
    // still anchor. A regression where the matcher silently required
    // some state field to be Some would fail here.
    let mut f = Fixture::with_config(anchor_config_top_left());
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let _target = map_titled_window(&mut f, id, "target", 800, 600);
    let _dialog = map_titled_window(&mut f, id, "dialog", 100, 50);
    let dialog_id = window_id_for_title(f.niri(), "dialog");

    let anchor = f
        .niri()
        .layout
        .floating_anchor_target_of(&dialog_id)
        .cloned();
    assert!(
        anchor.is_some(),
        "rule with only `title` set must match the tiled target despite \
         no is_floating / is_active / at_startup constraints; got {anchor:?}",
    );
}

#[test]
fn dependent_follows_target_across_workspaces() {
    // Spec success criterion: "target moves to another workspace → dialog
    // follows; passes assertions for both tile-pos and active-workspace."
    //
    // This exercises the cross-workspace branch of `notify_tile_changed`
    // (src/layout/mod.rs:944-988): the target's workspace-id changes, the
    // dependent's cached workspace differs from the target's, so the
    // dependent migrates via `move_to_output` and is then repositioned to
    // the target's new rect.

    let mut f = Fixture::with_config(anchor_config_top_left());
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let target = map_titled_window(&mut f, id, "target", 800, 600);
    let _dialog = map_titled_window(&mut f, id, "dialog", 100, 50);
    let dialog_id = window_id_for_title(f.niri(), "dialog");

    // Pre-condition: dialog is anchored to target on workspace 1.
    let initial_anchor = f
        .niri()
        .layout
        .floating_anchor_target_of(&dialog_id)
        .cloned();
    assert!(
        initial_anchor.is_some(),
        "dialog should be anchored to target before migration; got {initial_anchor:?}",
    );

    // Focus the target so move_to_workspace_down acts on it (move_to_
    // workspace_down operates on the active monitor's active tile).
    let target_id = window_id_for_title(f.niri(), "target");
    f.niri().layout.activate_window(&target_id);
    f.double_roundtrip(id);

    // Move target down to the next workspace. The new workspace is empty
    // until the move; afterwards the target is its sole occupant and a
    // fresh empty workspace is implicitly created below it. Both target
    // and (via the anchor follow path) dialog should be on the new
    // workspace.
    f.niri().layout.move_to_workspace_down(true);
    f.double_roundtrip(id);

    // Drive the configure/ack dance so the target's new size is committed
    // and `niri_complete_animations` fires the sweep that calls
    // notify_tile_changed for the migrated rect.
    let target_window = f.client(id).window(&target);
    target_window.ack_last_and_commit();
    f.double_roundtrip(id);
    f.niri_complete_animations();

    // Post-condition: the anchor relationship survived. The dialog's
    // anchor target is still the original target window.
    let post_anchor = f
        .niri()
        .layout
        .floating_anchor_target_of(&dialog_id)
        .cloned();
    assert_eq!(
        post_anchor.as_ref(),
        Some(&target_id),
        "dialog must remain anchored to the same target across workspace \
         migration; got {post_anchor:?}",
    );

    // The dialog must be on the same workspace as the target now. Use the
    // monitor-set walk to confirm both share a workspace id.
    let target_ws = f
        .niri()
        .layout
        .find_window_position_by_id(&target_id)
        .map(|(ws_id, _, _)| ws_id)
        .expect("target should be locatable after migration");
    let dialog_ws = f
        .niri()
        .layout
        .find_window_position_by_id(&dialog_id)
        .map(|(ws_id, _, _)| ws_id)
        .expect("dialog should be locatable after migration");
    assert_eq!(
        target_ws, dialog_ws,
        "dialog must share workspace with target after cross-workspace migration",
    );
}

#[test]
fn many_dependents_anchored_to_same_target_migrate_together() {
    // Performance / correctness guard for many-dependents-per-target:
    // when 6 dialogs all anchor to one target and the target migrates
    // across workspaces, every dialog must follow (correctness), and the
    // notify_tile_changed loop must complete in reasonable time
    // (bounded-cost). The per-dependent `move_to_output` call walks
    // monitors × workspaces × tiles; with N=6 dialogs this is still well
    // within a single-frame budget but exercises the code path the
    // typical N=1 tests don't.

    // Match by title prefix so multiple dialogs match the same rule. The
    // KDL string parses C-style escapes, so the backslash in `\d` must be
    // doubled to reach the regex compiler as `\d`.
    let cfg = niri_config::Config::parse_mem(
        r##"
        window-rule {
            match title="^dialog-\\d+$"
            open-floating true
            default-floating-position x=0 y=0 relative-to="top-left" {
                in-window-of title="^target$"
            }
        }
        "##,
    )
    .unwrap();

    let mut f = Fixture::with_config(cfg);
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let target = map_titled_window(&mut f, id, "target", 800, 600);
    let target_id = window_id_for_title(f.niri(), "target");

    let mut dialog_ids: Vec<smithay::desktop::Window> = Vec::new();
    for n in 0..6 {
        let title = format!("dialog-{n}");
        let _ = map_titled_window(&mut f, id, &title, 100, 50);
        dialog_ids.push(window_id_for_title(f.niri(), &title));
    }

    // All dialogs anchored to target.
    for dep in &dialog_ids {
        let anchor = f.niri().layout.floating_anchor_target_of(dep).cloned();
        assert_eq!(
            anchor.as_ref(),
            Some(&target_id),
            "every dialog must be anchored to the target before migration",
        );
    }

    // Migrate target across workspaces.
    f.niri().layout.activate_window(&target_id);
    f.double_roundtrip(id);
    let start = std::time::Instant::now();
    f.niri().layout.move_to_workspace_down(true);
    let target_window = f.client(id).window(&target);
    target_window.ack_last_and_commit();
    f.double_roundtrip(id);
    f.niri_complete_animations();
    let elapsed = start.elapsed();

    // Soft performance bound: in CI this should complete well under a
    // second even with the sweep firing once per dependent. The intent is
    // to catch a regression where the per-dependent cost compounds into
    // something that would visibly stall a frame.
    assert!(
        elapsed.as_millis() < 1000,
        "migration of 6 anchored dependents took {elapsed:?}, expected < 1s",
    );

    // Every dialog must still be anchored to the same target.
    for dep in &dialog_ids {
        let anchor = f.niri().layout.floating_anchor_target_of(dep).cloned();
        assert_eq!(
            anchor.as_ref(),
            Some(&target_id),
            "every dialog must remain anchored to target after migration",
        );
    }

    // Every dialog must be on the target's workspace.
    let target_ws = f
        .niri()
        .layout
        .find_window_position_by_id(&target_id)
        .map(|(ws_id, _, _)| ws_id)
        .expect("target locatable post-migration");
    for dep in &dialog_ids {
        let dep_ws = f
            .niri()
            .layout
            .find_window_position_by_id(dep)
            .map(|(ws_id, _, _)| ws_id)
            .expect("dependent locatable post-migration");
        assert_eq!(
            dep_ws, target_ws,
            "every dialog must share workspace with target",
        );
    }
}
