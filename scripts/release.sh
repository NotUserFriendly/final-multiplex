#!/usr/bin/env bash
# scripts/release.sh — cut a release: bump version, roll CHANGELOG, commit, tag.
#
# Usage:  scripts/release.sh X.Y.Z [--dry-run]
#
# Per ADR-0021 (SemVer, phase-driven minors). Run from a GREEN tree — the DoD
# checklist (fmt / clippy / test) is assumed already passed for the work being
# released. This script does NOT push; it prints the push command to run after
# you've reviewed the tagged commit.
set -euo pipefail

VERSION="${1:-}"
DRY_RUN=0
[[ "${2:-}" == "--dry-run" ]] && DRY_RUN=1

die() { echo "release: $*" >&2; exit 1; }

# ── args ──────────────────────────────────────────────────────────────────
[[ -n "$VERSION" ]] || die "usage: scripts/release.sh X.Y.Z [--dry-run]"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] \
  || die "'$VERSION' is not valid SemVer (X.Y.Z or X.Y.Z-pre)"
TAG="v$VERSION"
DATE="$(date -u +%Y-%m-%d)"

# ── must be at repo root with the files we edit ───────────────────────────
[[ -f Cargo.toml && -d .git ]] || die "run from the repository root"
[[ -f CHANGELOG.md ]]          || die "CHANGELOG.md not found"

# ── preconditions (fail fast) ─────────────────────────────────────────────
git diff --quiet && git diff --cached --quiet \
  || die "working tree is dirty — commit or stash first; the release commit must be clean"
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null \
  && die "tag $TAG already exists"
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
[[ "$BRANCH" == "main" ]] || echo "release: warning — on branch '$BRANCH', not 'main'" >&2

CUR="$(awk -F'"' '/^version[[:space:]]*=[[:space:]]*"/ {print $2; exit}' Cargo.toml)"
[[ -n "$CUR" ]]            || die "could not read current version from Cargo.toml"
[[ "$CUR" != "$VERSION" ]] || die "version is already $VERSION"

# [Unreleased] must exist and carry real content (no empty releases)
grep -q '^## \[Unreleased\]' CHANGELOG.md || die "no '## [Unreleased]' section in CHANGELOG.md"
BODY="$(awk '
  /^## \[Unreleased\]/ {inblk=1; next}
  inblk && /^## \[/    {exit}
  inblk                {print}
' CHANGELOG.md | grep -vE '^[[:space:]]*$|^<!--|^-->' || true)"
[[ -n "$BODY" ]] || die "[Unreleased] is empty — nothing to release"

echo "release: $CUR -> $VERSION   tag $TAG   date $DATE"

# ── edit: Cargo.toml workspace version (first 'version = \"...\"' line) ──────
awk -v v="$VERSION" '
  !done && /^version[[:space:]]*=[[:space:]]*"/ { sub(/"[^"]*"/, "\"" v "\""); done=1 }
  { print }
' Cargo.toml > Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml

# ── edit: CHANGELOG — rename [Unreleased] -> [X.Y.Z] - DATE, add fresh empty [Unreleased] ──
awk -v ver="$VERSION" -v date="$DATE" '
  !done && /^## \[Unreleased\][[:space:]]*$/ {
    print; print ""; print "## [" ver "] - " date
    done=1; next
  }
  { print }
' CHANGELOG.md > CHANGELOG.md.tmp && mv CHANGELOG.md.tmp CHANGELOG.md

# ── refresh Cargo.lock for the workspace version bump ─────────────────────
if command -v cargo >/dev/null 2>&1; then
  cargo update --workspace --quiet 2>/dev/null \
    || echo "release: warning — 'cargo update --workspace' failed; run 'cargo build' to refresh Cargo.lock" >&2
else
  echo "release: warning — cargo not found; Cargo.lock not refreshed" >&2
fi

# ── stage + show what's about to be committed ─────────────────────────────
git add Cargo.toml CHANGELOG.md
[[ -f Cargo.lock ]] && git add Cargo.lock || true
echo "─── staged for release ──────────────────────────────────────────"
git --no-pager diff --cached --stat
echo
git --no-pager diff --cached -- Cargo.toml CHANGELOG.md

# ── dry run stops here ────────────────────────────────────────────────────
if [[ "$DRY_RUN" == 1 ]]; then
  echo
  echo "release: --dry-run — edits staged, NOT committed. Review the diff above."
  echo "         discard with: git restore --staged --worktree Cargo.toml CHANGELOG.md Cargo.lock"
  exit 0
fi

# ── commit + annotated tag (LOCAL only — no push) ─────────────────────────
git commit -q -m "chore: release $VERSION"
git tag -a "$TAG" -m "Release $VERSION"
echo
echo "release: committed and tagged $TAG (local)."
echo "         push when ready:  git push && git push origin $TAG"
echo "         undo (local):     git tag -d $TAG && git reset --hard HEAD~1"
