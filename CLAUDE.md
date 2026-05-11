# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository identity

This is a personal fork of `YaLTeR/niri` (a scrollable-tiling Wayland compositor written in Rust on top of Smithay). The fork carries a stack of local patches plus an integration branch that combines them for desktop deployment.

- `origin` → `joshsymonds/niri`
- `upstream` → `YaLTeR/niri`

## Branch model

Three branch tiers, each with one job:

- **`main`** — fast-forward only from `upstream/main`. Never merge into it. Use `just sync-upstream` to update.
- **`josh/<topic>`** — feature/patch branches branched **directly off `main`**. Each holds one logically separable change. **The branch *is* the upstream PR** — push it and open a PR from `joshsymonds/niri:josh/<topic>` into `YaLTeR/niri:main`. No rebase/cleanup dance.
- **`josh/integration`** — the deploy artifact (see "Deploy model" below). It's `main` + tooling commits (`justfile`, `.envrc`, `CLAUDE.md`, `INTEGRATION.md`, `.gitignore`) + an octopus (or sequential) merge of every patch branch we want gnomon running today. **It is regenerated, not maintained**: when patches change or new ones land, hard-reset integration to main and re-run the recipe in "Re-deriving integration" below. The current set of merged patches and their upstream status is documented in `INTEGRATION.md` — keep it in sync when you re-derive. **If you're working in a `worktrees/<topic>/` worktree, see "Coordinating re-derivations" in `INTEGRATION.md` before force-pushing integration** — collisions between worktrees can drop other people's work.

When making edits, know which tier you're on: feature/patch work belongs on a `josh/<topic>` branch off `main`; tooling/docs commits belong on `josh/integration` only (and survive integration regeneration via cherry-pick).

### Why patches don't branch off integration

A previous iteration branched `josh/<topic>` off `josh/integration` (so they inherited tooling commits) and used a `prepare-pr` recipe to strip those commits at PR time via `git rebase --onto main josh/integration josh/<topic>`. That model existed because integration once held cargo-deploy plumbing patches needed to consume. After the deploy moved to a `niri-flake` input pin (see "Deploy model"), the inheritance stopped pulling its weight — and `prepare-pr`-by-default was just papering over a self-inflicted problem. Patches now branch off `main` directly. PRs are clean by construction. **Do not re-introduce the `pr/<topic>` concept or a `prepare-pr` recipe.**

### Deploy model (downstream consumers)

`josh/integration` is the deploy artifact for the user's NixOS desktop (`gnomon`). The user's `nix-config` repo pins it as a flake input:

```nix
# ~/nix-config/flake.nix
niri-flake = {
  url = "github:sodiboo/niri-flake";
  inputs.niri-unstable.url = "github:joshsymonds/niri/josh/integration";
};
```

`niri-flake`'s `make-niri` callPackage wraps that source through its full packaging pipeline (config validator, `niri-session` systemd unit, xwayland-satellite integration). `programs.niri.package` on gnomon resolves to this build.

**Canonical deploy flow:**
1. Land work on a `josh/<topic>` branch off `main` and push.
2. Re-derive `josh/integration` (see below) so it includes the new branch.
3. `git push -f origin josh/integration`.
4. In `~/nix-config`: `nix flake update niri-flake` → commit the lock bump → push.
5. On gnomon: `nixos-rebuild switch --flake ~/nix-config#gnomon` → restart niri (logout or `systemctl --user restart niri.service`).

**Always test stacked, never in isolation.** `nix-config`'s `niri-flake.inputs.niri-unstable.url` always points at `josh/integration`. To validate a patch, re-derive integration with that patch included on top of every other live patch and rebuild gnomon. Do NOT flip the input to a single patch branch for bisect/isolation testing — that hides interactions between patches. If you need to identify which of N patches caused a regression, drop suspects from the integration regen list one at a time, not by repointing the input.

### Re-deriving integration

Integration is regenerated from scratch every time the active patch set changes. There's no `rebase-integration` recipe because the inputs (which patches to merge) vary; the procedure is a few lines of plain git:

```sh
# 1. Capture the current tooling commit SHA so we can replay it.
TOOLING_SHA=$(git log josh/integration --format=%H -- justfile CLAUDE.md .envrc | head -1)

# 2. Reset integration to main.
git checkout josh/integration
git reset --hard main

# 3. Replay the tooling commit.
git cherry-pick "$TOOLING_SHA"

# 4. Octopus-merge whichever patch branches you want gnomon running.
#    The exact list lives in INTEGRATION.md — read it first to make sure
#    you don't drop a branch a parallel worktree just merged in.
git merge --no-ff \
    josh/cross-monitor-column-insert \
    josh/zoom-screencast-fix \
    josh/fullscreen-backdrop-clip \
    josh/focus-flash

# 5. Update INTEGRATION.md to reflect the merged set + upstream status.
$EDITOR INTEGRATION.md && git add INTEGRATION.md && git commit -m "INTEGRATION.md: refresh after regen"

# 6. Verify it builds, then force-push.
just build
git push -f origin josh/integration
```

If a patch is removed from gnomon's set, omit it from the merge list — that's the only "removal" mechanism. If patches conflict on the octopus merge, do them sequentially (`git merge a; git merge b; git merge c`) and resolve as you go.

### Tooling that was removed (do NOT re-introduce)

A previous iteration shipped ad-hoc cargo+scp deploy plumbing AND a `prepare-pr` workflow. **All of it has been deliberately removed** in favor of the flake-input deploy + branch-off-main model. If you find yourself wanting to re-add any of these, you're solving the wrong problem — bump the flake input and rebuild instead, or open a PR directly from a `josh/<topic>` branch.

Removed:
- `just dev-build` — `cargo build --release --bin niri` for local cargo-incremental builds.
- `just dev-deploy` — `scp target/release/niri gnomon:/tmp/niri-build/bin/`.
- `just setup-gnomon` — installed `~/.local/bin/niri-test` wrapper on gnomon.
- `just smoke-gnomon` — quick `niri --help` against the deployed binary.
- `just prepare-pr` — created `pr/<topic>` off main by stripping integration's tooling commits.
- `just rebase-integration` — replaced by the manual "Re-deriving integration" procedure above.
- `scripts/niri-test` — wrapper that ran `niri --session` from `~/.local/share/niri-test/current/bin/niri`.
- The `pr/<topic>` branch concept — `josh/<topic>` branches are now PR-ready directly.

The surviving recipes are fork-maintenance only:
- `just build` — local nix-build sanity check
- `just sync-upstream` — pull upstream main → push to fork's main
- `just rebase-patch <branch>` — keep a patch branch on top of latest main

## Build and dev environment

The `.envrc` uses `direnv` + the Nix flake (`flake.nix`) which provides a nightly Rust toolchain (required — `rustfmt` config uses unstable options) plus all native deps (libinput, libdrm, pipewire, pango, libdisplay-info, etc.). MSRV for plain `cargo check` is 1.85 (see CI `msrv` job), but day-to-day dev uses nightly.

- `just build` — `nix build`. Slow but reproducible.
- `cargo build` — fast iteration; relies on `pkg-config` finding system libs.
- Default cargo features: `dbus`, `systemd`, `xdp-gnome-screencast`. Other features: `dinit`, `profile-with-tracy`, `profile-with-tracy-ondemand`, `profile-with-tracy-allocations`.

## Test, lint, format

Run all of these before reporting a task complete (per the user's global preference for finishing in one pass):

- `cargo test --all --exclude niri-visual-tests` — full test suite. Some layout proptests are gated behind `RUN_SLOW_TESTS=1`.
- `RUN_SLOW_TESTS=1 PROPTEST_CASES=200000 PROPTEST_MAX_GLOBAL_REJECTS=200000 cargo test --release --all` — the heavy randomized run used in CI's `randomized-tests` job.
- `cargo clippy --all --all-targets` — must be clean.
- `cargo +nightly fmt --all` — required by CI; nightly rustfmt is non-optional because `rustfmt.toml` uses unstable options (`imports_granularity`, `group_imports`, `wrap_comments`).
- `cargo check --no-default-features` and feature-combo checks — CI's `build` job exercises every individual feature flag in isolation; if you touch `#[cfg(feature = ...)]` code, replicate locally.
- `cargo run --bin niri -- validate` — config validation for `default-config.kdl` or `~/.config/niri/config.kdl`.

To run a single test: `cargo test -p niri-config <name>` or `cargo test --test ... <name>`. Layout proptests live in `src/layout/tests.rs` + `src/layout/tests/`.

## Workspace layout

Cargo workspace with three sub-crates plus the main `niri` binary:

- `niri/` (root) — the compositor binary. Entry: `src/main.rs` → `niri::niri::State` running on a `calloop` event loop.
- `niri-config/` — KDL config schema and parser (uses `knuffel`). Lives independently because parsing has heavy build-time codegen.
- `niri-ipc/` — IPC types shared with clients (`niri msg`). Versioned along with niri; see its README — pin with `=X.Y.Z`.
- `niri-visual-tests/` — GTK + libadwaita app that exercises the real layout/render code with mock windows for visual inspection. Excluded from `cargo test --all` in CI.

## Architectural map

Multi-file concepts that aren't obvious from filenames:

**`src/niri.rs` is the central God-struct.** `State` owns the compositor: the Smithay protocol states, the `Layout`, all `OutputState`s, the IPC server, the d-bus servers, screencasting, etc. Most Wayland protocol traits are implemented on `State` either here or in `src/handlers/`. When investigating "where does X get plumbed in," start at `niri.rs`.

**`src/layout/` is the scrollable-tiling engine** and is the most complex subsystem. Hierarchy: `Layout` → `Monitor` (per output) → `Workspace` → `ScrollingSpace` (column strip) + `FloatingSpace` → `Column` → `Tile` → window. Read the docstring at the top of `src/layout/mod.rs` for the design principles around primary outputs and "original output" tracking. Layout has its own randomized property tests driven by an `Op` enum at the bottom of `src/layout/mod.rs` — when adding a layout action, add a variant to `Op` and (if applicable) the `every_op` arrays so it gets randomized coverage. `niri-visual-tests` is the visual counterpart to these tests.

**`src/handlers/` vs `src/protocols/`.** `handlers/` implements the standard Smithay-provided protocols (xdg-shell, layer-shell, compositor, background-effect). `protocols/` implements niri's own protocol code (foreign-toplevel, ext-workspace, gamma-control, screencopy, output-management, mutter-x11-interop, virtual-pointer).

**`src/backend/`** has three backends behind a `Backend` enum: `tty.rs` (DRM/udev/libinput, the real one), `winit.rs` (nested compositor for dev), `headless.rs` (used by tests via `src/tests/fixture.rs`).

**`src/input/`** owns the input pipeline + grabs (move, resize, swipe, picker grabs). `input/mod.rs` is the dispatcher; `*_grab.rs` files are stateful interactions implemented as Smithay grabs.

**`src/render_helpers/`** is the rendering toolkit on top of Smithay's GLES renderer — custom shaders (border, blur, shadow, gradient-fade), offscreen/effect buffers, snapshots, damage helpers. `niri_render_elements!` macro generates the per-context render-element enum.

**`src/dbus/`** is feature-gated on `dbus`; `screencasting/` is gated on `xdp-gnome-screencast`. `a11y.rs` is gated on `dbus` and uses `accesskit_unix`.

**`src/tests/`** runs an end-to-end client/server harness (`client.rs`, `server.rs`, `fixture.rs`) — actual Wayland clients against a real `State` driven by the headless backend. Use this for protocol/lifecycle bugs that the layout proptests can't catch.

**IPC.** `niri-ipc` defines the wire types. `src/ipc/server.rs` (compositor side) and `src/ipc/client.rs` (`niri msg` side) speak over a Unix socket whose path is exported as `NIRI_SOCKET` to children.

## Logging conventions

Per `docs/wiki/Development:-Developing-niri.md`, levels carry meaning that's checked in review:

- `error!` — recoverable bug. ERROR in the log = a bug, always.
- `warn!` — user/hardware/config issue. Config parse errors go here.
- `info!` — important normal-operation messages. `RUST_LOG=niri=info` should not be noisy.
- `debug!` — normal-operation detail.
- `trace!` — spammy/perf-sensitive. Compiled out of release builds.

## Fork-specific gotchas

- `josh/integration` carries tooling commits (`justfile`, `.envrc`, `CLAUDE.md`, `INTEGRATION.md`, `.gitignore`) that must NOT land in upstream PRs. Patch branches branch off `main` precisely so they never inherit them.
- When updating from upstream: `just sync-upstream`, then `just rebase-patch <branch>` for each patch branch on top of new `main`, then re-derive `josh/integration` per the recipe above.
- The Nix package expression is community-maintained (header in `flake.nix`); upstream PRs touching it should be coordinated.
- **Do NOT add ad-hoc deploy recipes to the justfile or scripts to this repo.** Deploy = bump `niri-flake.inputs.niri-unstable` in `~/nix-config` + `nixos-rebuild` on gnomon. See "Deploy model" above. The justfile recipes that exist here are *fork-maintenance only* (sync, rebase-patch).
- **Do NOT add new config keys without checking the validator.** When adding a new option to `niri-config`'s `LayoutPart`/window rules/etc., the FIRST consumer to break is `niri-flake`'s config validator on rebuild. The fix is to push the niri change to `josh/integration` first (which means landing the patch branch, then re-deriving integration to include it), bump the flake input, rebuild — the validator uses our binary, so the new key is recognized. If you add the config in nix-config first (without bumping the flake input), gnomon's rebuild fails with "unknown node" against the upstream validator binary.
