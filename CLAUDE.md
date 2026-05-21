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
- **`josh/integration`** — the deploy artifact (see "Deploy model" below). It's `main` + persistent tooling commits (`justfile`, `.envrc`, `CLAUDE.md`, `INTEGRATION.md`, `.gitignore`) + a maintained stack of `--no-ff` merges of every patch branch we want gnomon running today. **It is maintained, not regenerated**: add or update a patch by merging its branch in and resolving conflicts once — the resolution is a durable commit that exists on every clone, with no machine-local `rr-cache` and no reset-to-`main` re-derivation. Remove a patch by reverting its merge commit. The merged set + upstream status is documented in `INTEGRATION.md` — update it in the same change. Run `just integration-check` (the oracle gate) before every build/push: the integration delta must be exactly the merged patch branches' own deltas, nothing else. **If a parallel worktree/machine also maintains integration, `git pull --ff-only` then merge into it — never reset/force a regenerated tree over it; that silently drops resolutions (see the 2026-05 rerere-stale incident: replaying a stale cross-machine `rr-cache` reverted `render-above-fullscreen` + `focus-flash` work, caught only by the oracle diff).** Patch branches still branch off `main`, so upstream PRs stay clean by construction.

When making edits, know which tier you're on: feature/patch work belongs on a `josh/<topic>` branch off `main`; tooling/docs commits belong on `josh/integration` only (they just live on the branch — see "Maintaining integration" below).

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
2. Merge the branch into `josh/integration` per "Maintaining integration" below (`git merge --no-ff josh/<topic>`, update `INTEGRATION.md`, run `just integration-check`).
3. `git push origin josh/integration` (plain push — fast-forward; `-f` only for history surgery).
4. In `~/nix-config`: `nix flake update niri-flake` → commit the lock bump → push.
5. On gnomon: `nixos-rebuild switch --flake ~/nix-config#gnomon` → restart niri (logout or `systemctl --user restart niri.service`).

**Always test stacked, never in isolation.** `nix-config`'s `niri-flake.inputs.niri-unstable.url` always points at `josh/integration`. To validate a patch, merge it into integration on top of every other live patch and rebuild gnomon. Do NOT flip the input to a single patch branch for bisect/isolation testing — that hides interactions between patches. If you need to identify which of N patches caused a regression, revert suspect merge commits one at a time (`git revert -m 1 <merge-sha>`), not by repointing the input.

### Maintaining integration

`josh/integration` is long-lived. Conflict resolutions are durable merge
commits — never re-derived, never machine-local. Operations:

```sh
git checkout josh/integration
git pull --ff-only origin josh/integration   # never clobber a parallel maintainer

# add or update a patch (re-merging an updated branch only conflicts on
# its new commits — small):
git merge --no-ff josh/<topic>

# sync upstream:
git merge --no-ff main

# remove a patch (rare — the one awkward op):
git revert -m 1 <merge-commit-of-that-patch>

# update INTEGRATION.md in the same change as any set change:
$EDITOR INTEGRATION.md && git add INTEGRATION.md \
    && git commit -m "INTEGRATION.md: <what changed>"

just integration-check                       # ORACLE GATE — must pass
just build                                   # nix sanity
git push origin josh/integration             # plain push; -f only for history surgery
```

Resolve conflicts preserving **all** sides — never resolve by dropping
a patch's content. `just integration-check` fails the change if the
integration delta touches anything outside the merged branches' own
deltas, which is exactly how a stale or wrong resolution is caught
*before* it ships (the 2026-05 rerere-stale incident shipped nothing
only because this diff was run by hand; it is now a gate).

There is no reset-to-`main` regeneration and no tooling cherry-pick
dance: tooling commits simply live on the branch. If `git push` is
rejected because a parallel maintainer advanced origin, `git pull
--ff-only` (or merge), re-run `just integration-check` + `just build`,
then push again. Patch branches still branch off `main` and remain the
clean upstream PR unit.

### Tooling that was removed (do NOT re-introduce)

A previous iteration shipped ad-hoc cargo+scp deploy plumbing AND a `prepare-pr` workflow. **All of it has been deliberately removed** in favor of the flake-input deploy + branch-off-main model. If you find yourself wanting to re-add any of these, you're solving the wrong problem — bump the flake input and rebuild instead, or open a PR directly from a `josh/<topic>` branch.

Removed:
- `just dev-build` — `cargo build --release --bin niri` for local cargo-incremental builds.
- `just dev-deploy` — `scp target/release/niri gnomon:/tmp/niri-build/bin/`.
- `just setup-gnomon` — installed `~/.local/bin/niri-test` wrapper on gnomon.
- `just smoke-gnomon` — quick `niri --help` against the deployed binary.
- `just prepare-pr` — created `pr/<topic>` off main by stripping integration's tooling commits.
- `just rebase-integration` — there is no re-derivation; integration is maintained (see "Maintaining integration").
- `scripts/niri-test` — wrapper that ran `niri --session` from `~/.local/share/niri-test/current/bin/niri`.
- The `pr/<topic>` branch concept — `josh/<topic>` branches are now PR-ready directly.
- The reset-to-`main` / cherry-pick-tooling / octopus regeneration ritual, and any `rr-cache` sharing between machines — replaced by the maintained-branch model + `just integration-check`. Regeneration has no durable home for conflict resolutions and silently drops work across machines (2026-05 rerere-stale incident). Do not reintroduce "regenerate integration from scratch".

The surviving recipes are fork-maintenance only:
- `just build` — local nix-build sanity check
- `just sync-upstream` — pull upstream main → push to fork's main
- `just rebase-patch <branch>` — keep a patch branch on top of latest main
- `just integration-check` — oracle gate; asserts the integration delta is exactly the merged patch branches' own deltas (run before every build/push)

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
- When updating from upstream: `just sync-upstream`, then `just rebase-patch <branch>` for each patch branch on top of new `main`, then `git merge --no-ff main` into `josh/integration` and re-merge any patch branches that changed (see "Maintaining integration" above).
- The Nix package expression is community-maintained (header in `flake.nix`); upstream PRs touching it should be coordinated.
- **Do NOT add ad-hoc deploy recipes to the justfile or scripts to this repo.** Deploy = bump `niri-flake.inputs.niri-unstable` in `~/nix-config` + `nixos-rebuild` on gnomon. See "Deploy model" above. The justfile recipes that exist here are *fork-maintenance only* (sync, rebase-patch).
- **Do NOT add new config keys without checking the validator.** When adding a new option to `niri-config`'s `LayoutPart`/window rules/etc., the FIRST consumer to break is `niri-flake`'s config validator on rebuild. The fix is to push the niri change to `josh/integration` first (land the patch branch, then `git merge --no-ff` it into integration per "Maintaining integration" above), bump the flake input, rebuild — the validator uses our binary, so the new key is recognized. If you add the config in nix-config first (without bumping the flake input), gnomon's rebuild fails with "unknown node" against the upstream validator binary.
