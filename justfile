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

# Oracle gate for the maintained josh/integration. Asserts the code delta
# of HEAD vs the last pushed integration is a SUBSET of the union of the
# merged patch branches' own deltas vs main — i.e. a merge resolution
# cannot have touched any file no patch branch touches. Catches stale /
# wrong conflict resolutions before build/push. Run before every push.
integration-check:
    #!/usr/bin/env bash
    set -euo pipefail
    git fetch origin --quiet
    prev=origin/josh/integration
    # tooling files legitimately diverge from prev; exclude them.
    git diff --name-only "$prev" HEAD -- \
        ':!CLAUDE.md' ':!justfile' ':!INTEGRATION.md' ':!.envrc' ':!.gitignore' \
        | sort -u > /tmp/niri-int.delta
    : > /tmp/niri-pat.delta
    for b in $(git log --merges --format=%s "main..HEAD" \
                 | grep -oE 'josh/[a-z0-9-]+' \
                 | grep -vx 'josh/integration' | sort -u); do
        ref="origin/$b"
        git rev-parse --verify --quiet "$ref" >/dev/null || ref="$b"
        mb=$(git merge-base main "$ref" 2>/dev/null) || continue
        git diff --name-only "$mb" "$ref"
    done | sort -u > /tmp/niri-pat.delta
    extra=$(comm -23 /tmp/niri-int.delta /tmp/niri-pat.delta || true)
    if [ -n "$extra" ]; then
        echo "ORACLE FAIL — integration changed files no patch branch touches:"
        echo "$extra" | sed 's/^/  /'
        echo "A merge resolution likely dropped or mangled patch content. Do NOT push."
        exit 1
    fi
    echo "ORACLE OK — integration code delta ⊆ patch deltas ($(wc -l < /tmp/niri-int.delta | tr -d ' ') files vs $prev)"
