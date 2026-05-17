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
    int_delta=$(mktemp); pat_delta=$(mktemp)
    trap 'rm -f "$int_delta" "$pat_delta"' EXIT
    git fetch origin --quiet
    prev=origin/josh/integration
    # tooling files legitimately diverge from prev; exclude them.
    git diff --name-only "$prev" HEAD -- \
        ':!CLAUDE.md' ':!justfile' ':!INTEGRATION.md' ':!.envrc' ':!.gitignore' \
        | sort -u > "$int_delta"
    # Branch list is parsed from merge-commit SUBJECTS, so this relies on the
    # default `Merge branch 'josh/<topic>'` subject (or any subject containing
    # the branch name). A merge made with a custom `-m` message that omits the
    # branch name is excluded from the union, which only SHRINKS pat-delta and
    # makes the gate flag more readily — fail-closed, never fail-open.
    for b in $(git log --merges --format=%s "main..HEAD" \
                 | grep -oE 'josh/[a-z0-9-]+' \
                 | grep -vx 'josh/integration' | sort -u); do
        ref="origin/$b"
        git rev-parse --verify --quiet "$ref" >/dev/null || ref="$b"
        mb=$(git merge-base main "$ref" 2>/dev/null) || continue
        git diff --name-only "$mb" "$ref"
    done | sort -u > "$pat_delta"
    extra=$(comm -23 "$int_delta" "$pat_delta" || true)
    if [ -n "$extra" ]; then
        echo "ORACLE FAIL — integration changed files no patch branch touches:"
        echo "$extra" | sed 's/^/  /'
        echo "A merge resolution likely dropped or mangled patch content. Do NOT push."
        exit 1
    fi
    echo "ORACLE OK — integration code delta ⊆ patch deltas ($(wc -l < "$int_delta" | tr -d ' ') files vs $prev)"
