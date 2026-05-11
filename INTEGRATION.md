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
