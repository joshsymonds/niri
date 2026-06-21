//! Behavioral tests for `wp_pointer_warp_v1` support.
//!
//! Setup: a single-output fixture with one mapped window. We center the
//! cursor on the window via `move_cursor_to_focused_tile` so the window's
//! surface holds pointer focus, then drive `State::warp_pointer_to`
//! directly. The helper returns `true` when it warps the pointer and
//! `false` when it rejects the request (surface not focused, or position
//! outside the surface); we assert on that signal plus the resulting
//! pointer location.

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point};

use super::*;
use crate::niri::CenterCoords;

/// Map a single `size`×`size` window and center the cursor on it so its
/// surface holds pointer focus. Returns the fixture, the server-side
/// surface, and the surface's origin in the global coordinate space.
fn map_focused_window(size: u16) -> (Fixture, WlSurface, Point<f64, Logical>) {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));

    let id = f.add_client();
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);

    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.set_size(size, size);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    // Put the cursor on the window so its surface gets pointer focus.
    let warped = f
        .niri_state()
        .move_cursor_to_focused_tile(CenterCoords::Both);
    assert!(
        warped,
        "fixture precondition: cursor should warp onto the mapped window",
    );

    let (srv_surface, origin) = f
        .niri()
        .pointer_contents
        .surface
        .clone()
        .expect("the mapped window's surface should hold pointer focus");

    (f, srv_surface, origin)
}

#[test]
fn warp_moves_pointer_for_focused_surface() {
    let (mut f, surface, origin) = map_focused_window(100);

    let warped = f
        .niri_state()
        .warp_pointer_to(&surface, Point::from((10.0, 20.0)));
    assert!(warped, "warp on the focused surface should succeed");

    let loc = f.niri().seat.get_pointer().unwrap().current_location();
    assert_eq!(
        loc,
        origin + Point::from((10.0, 20.0)),
        "pointer should land at the surface origin plus the requested offset",
    );
}

#[test]
fn warp_rejected_when_position_out_of_bounds() {
    let (mut f, surface, _origin) = map_focused_window(100);
    let before = f.niri().seat.get_pointer().unwrap().current_location();

    // The surface is 100×100; (500, 500) is outside it.
    let warped = f
        .niri_state()
        .warp_pointer_to(&surface, Point::from((500.0, 500.0)));
    assert!(
        !warped,
        "warp outside the surface bounds should be rejected"
    );

    let after = f.niri().seat.get_pointer().unwrap().current_location();
    assert_eq!(before, after, "a rejected warp must not move the pointer");
}

#[test]
fn warp_rejected_when_surface_not_focused() {
    let (mut f, surface, _origin) = map_focused_window(100);

    // Move the cursor off the window into empty space, dropping pointer focus.
    f.niri_state().move_cursor(Point::from((1900.0, 1060.0)));
    assert!(
        f.niri().pointer_contents.surface.is_none(),
        "precondition: the cursor should be over empty space",
    );
    let before = f.niri().seat.get_pointer().unwrap().current_location();

    let warped = f
        .niri_state()
        .warp_pointer_to(&surface, Point::from((10.0, 20.0)));
    assert!(
        !warped,
        "warp from a surface without pointer focus must be rejected",
    );

    let after = f.niri().seat.get_pointer().unwrap().current_location();
    assert_eq!(before, after, "a rejected warp must not move the pointer");
}
