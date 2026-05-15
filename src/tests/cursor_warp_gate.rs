//! Behavioral tests for the `block-focus-cursor-warp` window-rule gate.
//!
//! Setup: a single-output fixture, one mapped window. We drive
//! `State::move_cursor_to_focused_tile` directly so we don't depend on
//! the `warp_mouse_to_focus` config (its own outer guard would otherwise
//! short-circuit before we reach the new gate).
//!
//! The gate's return-value contract: `move_cursor_to_focused_tile`
//! returns `true` when it actually moved the cursor, `false` when it
//! short-circuited. We use that as the observable signal — no need to
//! peek at the cursor's actual screen coordinates.

use niri_config::Config;

use super::*;
use crate::niri::CenterCoords;

/// Map a single 100×100 window with the given title and wait for it to
/// become the active focused tile. Returns the fixture so the test can
/// call into `f.niri_state()` for the move-cursor-to-focused-tile
/// assertion; the client/surface handles are internal scaffolding the
/// tests don't need.
fn map_titled_window(config: Config, title: &str) -> Fixture {
    let mut f = Fixture::with_config(config);
    f.add_output(1, (1920, 1080));

    let id = f.add_client();
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.set_title(title);
    window.commit();
    f.roundtrip(id);

    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.set_size(100, 100);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    f
}

#[test]
fn gate_fires_for_matched_window() {
    // A window-rule sets `block-focus-cursor-warp true` for any window
    // titled "toolbar". The mapped window matches, so the gate inside
    // `move_cursor_to_focused_tile` MUST short-circuit and return false
    // — even though the cursor would otherwise warp to the focused tile.
    let config = Config::parse_mem(
        r##"
        window-rule {
            match title="^toolbar$"
            block-focus-cursor-warp true
        }
        "##,
    )
    .unwrap();
    let mut f = map_titled_window(config, "toolbar");

    let warped = f
        .niri_state()
        .move_cursor_to_focused_tile(CenterCoords::Separately);
    assert!(
        !warped,
        "expected the gate to short-circuit the warp for a matched window",
    );
}

#[test]
fn gate_does_not_fire_when_rule_absent() {
    // No matching window-rule, so `block_focus_cursor_warp` stays as
    // `None` on the resolved rules. The gate must NOT short-circuit —
    // the warp proceeds and returns true (cursor moved to the tile).
    // This is the regression guard: a future refactor that mis-resolves
    // the rule's default would otherwise silently break cursor-follows-
    // focus for every unrelated window.
    let mut f = map_titled_window(Config::default(), "ordinary");

    let warped = f
        .niri_state()
        .move_cursor_to_focused_tile(CenterCoords::Separately);
    assert!(warped, "expected the warp to proceed when no rule applies",);
}

#[test]
fn gate_does_not_fire_when_rule_set_to_false() {
    // Some(false) is explicit "do warp" — the same as `None` for our
    // purposes. Users would write `false` to override a more-general
    // rule that set `true`. The gate's `== Some(true)` check ensures
    // only the explicit "yes block" value fires.
    let config = Config::parse_mem(
        r##"
        window-rule {
            match title="^ordinary$"
            block-focus-cursor-warp false
        }
        "##,
    )
    .unwrap();
    let mut f = map_titled_window(config, "ordinary");

    let warped = f
        .niri_state()
        .move_cursor_to_focused_tile(CenterCoords::Separately);
    assert!(
        warped,
        "expected the warp to proceed when the rule explicitly sets false",
    );
}

#[test]
fn gate_does_not_fire_when_rule_does_not_match() {
    // A non-matching rule sets `block-focus-cursor-warp true` for a
    // different title pattern. The mapped window has title "toolbar"
    // which does NOT match `^other$`, so the resolution loop in
    // `ResolvedWindowRules::compute` skips the rule entirely. The gate
    // must not fire.
    //
    // This catches a regression where the `block_focus_cursor_warp`
    // merge is moved outside the `if !(rule.matches.is_empty() || ...)`
    // gate, which would let any rule's `Some(true)` leak across to
    // unrelated windows. The "absent rule" test alone doesn't catch
    // that — its config has no rules at all so the loop body is
    // unreachable.
    let config = Config::parse_mem(
        r##"
        window-rule {
            match title="^other$"
            block-focus-cursor-warp true
        }
        "##,
    )
    .unwrap();
    let mut f = map_titled_window(config, "toolbar");

    let warped = f
        .niri_state()
        .move_cursor_to_focused_tile(CenterCoords::Separately);
    assert!(
        warped,
        "expected the warp to proceed when the rule matcher doesn't apply to this window",
    );
}
