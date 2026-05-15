# Integration manifest

This file documents what `josh/integration` currently includes. It's a
human-maintained snapshot that lives on integration only (not on patch
branches or main), regenerated whenever integration is.

When you re-derive integration via the recipe in `CLAUDE.md`, **update
this file** so the next reader (human or agent) can tell at a glance
what's deployed without spelunking the merge graph.

## Patch branches in the current integration

| Branch | Origin | Upstream status | What it does |
|---|---|---|---|
| `josh/cross-monitor-column-insert` | local | Not yet PRed upstream | Adds a `cross-monitor-column-insert` config option to control where `MoveColumnTo<Direction>Monitor` lands a column on the destination monitor (`after-active` vs `adjacent`). |
| `josh/zoom-screencast-fix` | Merge of [niri-wm/niri#1791](https://github.com/niri-wm/niri/pull/1791) by `wrvsrx` + downstream polish | PR open; YaLTeR has reviewed structure and committed to merging when reviewer bandwidth permits | Adds SHM-fallback to the PipeWire screencast pipeline so xdg-desktop-portal-gnome consumers (Zoom, Slack, Chromium/Electron) that don't speak DMA-BUF can negotiate. Without this, those apps get a blank screen-share on niri+NVIDIA. |
| `josh/fullscreen-backdrop-clip` | local | Not yet PRed upstream | Clips the fullscreen backdrop to an L-shape (tile minus window) so transparent fullscreen windows (kitty with `background_opacity<1.0`) show wallpaper through the gaps instead of opaque black. |
| `josh/focus-flash` | local | Not yet PRed upstream | Brief color flash on a window's focus ring/border when focus arrives, configured via a new `layout.focus-flash` block (flash-color, pulse-duration, pulses, edge-width, sides). Helps visually track where keyboard focus went on a busy multi-monitor layout. |
| `josh/ffm-edge-deadzone` | local | Not yet PRed upstream | Adds an `edge-deadzone` property (logical pixels) to `focus-follows-mouse`. When set, FFM is suppressed if the cursor is within N pixels of any edge of the candidate window or on its niri border — the cursor must commit into the window's interior. Orthogonal to `max-scroll-amount`. Also changes the FFM same-check to compare against the layout's keyboard-focused window rather than the cursor's previous geometric window, so the deadzone re-evaluates on every motion until the cursor commits past it. |
| `josh/block-pointer-constraints` | local | [PR #4045](https://github.com/niri-wm/niri/pull/4045) open upstream | Adds a `block-pointer-constraints` window-rule (`Option<bool>`) that suppresses activation of `zwp_pointer_constraints_v1` for matching windows. Both `Locked` and `Confined` variants are suppressed unconditionally — the rule is opt-in per window. Gate is placed inside the `with_pointer_constraint` callback so the no-constraint fast path (every pointer motion over a surface without a constraint) is unchanged. Motivated by Zoom's `annotate_toolbar` overlay locking the cursor when crossed; specific apps that legitimately need pointer-lock (games, drawing tablets) are unaffected unless explicitly matched. |
| `josh/render-above-fullscreen` | local | Pushed to `origin`; upstream PR not yet filed | Adds a `render-above-fullscreen` window-rule (`Option<bool>`) that promotes a floating tile's render order from below-scrolling (current default, gets hidden behind fullscreen windows) to above-scrolling (renders on top of fullscreen). Implementation: new `FloatingRenderPass` enum, two-pass call to `Workspace::render_floating` in `Monitor::render_workspaces` with `AboveFullscreen` emitted first (smithay paints front-to-back, so first-emitted = topmost). The `AboveFullscreen` pass bypasses the `is_floating_visible()` early-return so flagged tiles render even when fullscreen is focused; `Workspace::window_under` parallel-handles the flagged subset for hit-test parity. Motivated by Zoom's `as_toolbar` and `zoom_linux_float_video_window` helpers losing visibility when the user fullscreens an unrelated app; generalizable to any always-on-top utility window (PiP video, annotation overlays). |
| `josh/floating-position-center` | local | Pushed to `origin`; upstream PR not yet filed | Adds `Center` to the `RelativeTo` enum on `default-floating-position`'s `relative-to` property. Centers a floating tile on both axes within the working area when `relative-to="center"` (matching the natural reading; previously you had to write `relative-to="top"` and then offset y to fake a center). One-commit precursor patch; the `josh/relative-to-window-config` branch carries the same `Center` work as part of a larger feature, so this branch's primary role is to provide the standalone PR shape if it ever upstreams. |
| `josh/relative-to-window-config` | local | Pushed to `origin`; upstream PR not yet filed | Adds `in-window-of` child block on `default-floating-position` that anchors a floating window inside another window's tile rectangle (resolved at dialog-map time via a layered Match filter: same-workspace → same-output → MRU). Maintains a reverse-indexed `AnchorIndex<TileId>` on `Layout` so dependents follow the target's geometry/workspace/output changes via a per-frame sweep in `advance_animations`. User-drag of the dependent breaks the anchor; target close orphans dependents (no re-resolve — MRU-at-open-time). Motivated by Zoom's "Leave meeting panel" floating dialog opening at working-area center where it straddles tile boundaries; the rule lets users anchor it inside the Meeting tile via `in-window-of title="^Meeting$"`. |
| `josh/block-focus-cursor-warp` | local | Pushed to `origin`; upstream PR not yet filed | Adds a `block-focus-cursor-warp` window-rule (`Option<bool>`) that suppresses niri's cursor-follows-focus warp for matching windows. Gate placed inside `State::move_cursor_to_focused_tile` after the existing early-returns: if the active window's resolved rule has `block_focus_cursor_warp == Some(true)`, the function short-circuits before computing the destination rect. Single chokepoint covers both `maybe_warp_cursor_to_focus` and `maybe_warp_cursor_to_focus_centered` (and any future warp wrapper). Motivated by Zoom's `annotate_toolbar` overlay (a 112×112 floating xdg_toplevel during screen-share-with-annotation) repeatedly calling `xdg_activation_v1.activate()` ~30Hz, which routed through the cursor-follows-focus warp and snapped the cursor to the toolbar's (56, 56) center every time the user moved the mouse. Opt-in per window, so apps that legitimately benefit from cursor warp on activation (notifications, freshly-launched apps) are unaffected unless explicitly matched. |

## Tooling commit

Single commit at the base of integration (after `main`) carries
everything that should NOT bleed into upstream PRs:

- `justfile` — fork-maintenance recipes (`build`, `sync-upstream`, `rebase-patch`)
- `.envrc` — direnv glue to load the flake's nightly Rust + native deps
- `CLAUDE.md` — agent guide explaining the branch model + re-derivation
- `INTEGRATION.md` — this file
- `.gitignore` — adds `/worktrees`, `/reference` (out-of-tree dirs for git
  worktrees and the prior-art reference checkouts)

## reference/

The (gitignored) `reference/` directory holds shallow clones of other
projects whose code we use as reference when implementing or validating
patches. It's not part of any commit. Current contents:

- `mutter/` — GNOME's Wayland compositor; canonical xdg-desktop-portal-gnome
  producer. See `src/backends/meta-screen-cast-stream-src.c` for the
  reference DMA-BUF + SHM screencast implementation
  ([MR !1939](https://gitlab.gnome.org/GNOME/mutter/-/merge_requests/1939)).
- `kwin/` — KDE's Wayland compositor. See
  `src/plugins/screencast/screencaststream.cpp` for the C++-flavored
  reference, especially `buildFormats()` for the multi-pod pattern
  ([MR !1210](https://invent.kde.org/plasma/kwin/-/merge_requests/1210)).
- `kpipewire/` — KDE's PipeWire helper library. Consumer-side mirror of
  the same pod-construction pattern; useful for cross-checking version
  gates (`kDmaBufModifierMinVersion = {0, 3, 33}`).
- `xdg-desktop-portal-wlr/` — wlroots-family screencast portal. Pure C
  reference for the two-pod offering pattern in `src/screencast/pipewire_screencast.c`.
- `xdg-desktop-portal-hyprland/` — Hyprland's portal fork. Cleanest
  example of explicit `force_shm` and DMA-failure renegotiate logic
  in `src/portals/Screencopy.cpp`.
- `niri-pr-1791/*.patch` — the 19-commit `git format-patch` series of
  PR #1791, preserved as flat patch files for archaeology.

## Coordinating re-derivations across worktrees

If you're working in a `worktrees/<topic>/` subdir, your re-derivation
of `josh/integration` may collide with another worktree's. Always
deploy a patch by re-deriving integration on top of every other live
patch — never point `nix-config`'s `niri-flake` input at a single
patch branch. Testing a patch means testing it stacked with every
other deployed patch, not in isolation. Before you force-push
integration:

- Pull `origin/josh/integration` first, identify what's currently
  merged, and include those branches in your regen — don't drop work
  that's already deployed.
- Update this file in the same commit so the next reader sees the truth.

## Re-deriving integration

See "Re-deriving integration" in `CLAUDE.md`. The procedure is unchanged
from this manifest; this file just makes the *current* state legible.

When patches are added or removed:
1. Re-run the recipe in `CLAUDE.md`.
2. Update this file's table to match.
3. Force-push integration.
