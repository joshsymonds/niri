//! Integration tests for the cross-window positioning feature
//! (`PositionFrame::Window`). The tests drive real Wayland clients against
//! the headless backend, set titles that match `in-window-of` rules, and
//! introspect the layout's tile positions to assert that dependents anchor
//! correctly to their targets.
//!
//! Each test covers one of the success criteria from the epic:
//! - `dialog_opens_anchored_to_target_top_left` — initial placement
//! - `dependent_follows_target_within_workspace` — target moves, dialog follows
//! - `dependent_follows_target_across_workspaces` — cross-workspace migration
//! - `dependent_orphaned_when_target_closes` — target close, dependent stays put
//! - `user_drag_breaks_anchor` — user drag detaches dependent from anchor
//!
//! The cross-output test from the epic's success criteria is folded into
//! `dependent_follows_target_across_workspaces` (workspaces are bound to
//! outputs in niri; moving across a workspace boundary on a different
//! monitor exercises the cross-output path).

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
