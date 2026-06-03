#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/sync-fork.sh [branch]

Sync the fork remote from the upstream remote.

Defaults:
  branch: main
  fork remote: origin
  upstream remote: upstream

Environment overrides:
  FORK_REMOTE=origin
  UPSTREAM_REMOTE=upstream

The script always:
  1. fetches UPSTREAM_REMOTE/branch
  2. pushes that exact commit to FORK_REMOTE/branch
  3. fetches FORK_REMOTE/branch

It only fast-forwards the local branch when:
  - the current branch is the target branch, and
  - the worktree is clean.
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

branch="${1:-main}"
fork_remote="${FORK_REMOTE:-origin}"
upstream_remote="${UPSTREAM_REMOTE:-upstream}"

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

require_remote() {
  local remote="$1"
  if ! git remote get-url "$remote" >/dev/null 2>&1; then
    echo "error: remote '$remote' is not configured" >&2
    echo >&2
    git remote -v >&2
    exit 1
  fi
}

require_remote "$fork_remote"
require_remote "$upstream_remote"

echo "Fetching $upstream_remote/$branch..."
git fetch "$upstream_remote" "$branch"

upstream_ref="$upstream_remote/$branch"
upstream_commit="$(git rev-parse "$upstream_ref")"

echo "Updating $fork_remote/$branch to $upstream_commit..."
git push "$fork_remote" "$upstream_ref:refs/heads/$branch"

echo "Fetching $fork_remote/$branch..."
git fetch "$fork_remote" "$branch"

current_branch="$(git branch --show-current)"
if [[ "$current_branch" != "$branch" ]]; then
  echo "Synced fork. Local checkout is on '$current_branch', so local '$branch' was not updated."
  exit 0
fi

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "Synced fork. Local '$branch' was not fast-forwarded because the worktree has changes."
  echo "After committing or stashing, run: git pull --ff-only $fork_remote $branch"
  exit 0
fi

echo "Fast-forwarding local $branch..."
git merge --ff-only "$fork_remote/$branch"

echo "Done. $fork_remote/$branch and local $branch are synced to $upstream_commit."
