default:
    @just --list

# Build niri locally via the flake. Slow but reproducible.
build:
    nix build

# Pull upstream main into local main (fast-forward only). Never merge.
sync-upstream:
    git fetch upstream
    git checkout main
    git merge --ff-only upstream/main
    git push origin main
    @echo "main is now at $(git rev-parse --short main)"

# Rebase a patch branch on latest main. Patch branches branch off main
# (not josh/integration) so they stay PR-ready by construction.
rebase-patch branch:
    git checkout {{branch}}
    git rebase main
    @echo "Rebased {{branch}} on $(git rev-parse --short main). Force-push when ready."
