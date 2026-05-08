# Releasing AnyAI

One command:

```bash
just release 0.1.8
```

That's it. The recipe runs `scripts/bump-version.sh`, commits any diff, pushes, then dispatches `release.yml` for the requested tag.

## What `just release X.Y.Z` does

1. **Bumps versions** via `scripts/bump-version.sh`:
   - `src-tauri/Cargo.toml` — `[package]` version (single source of truth).
   - `src-tauri/Cargo.lock` — the `[[package]]` entry whose `name = "anyai"`.
   - `package.json` — `version` field (kept in sync for tooling).
   - A leading `v` is accepted (`just release v0.1.8`) and stripped.
2. **Commits** `chore(release): X.Y.Z` if any of the three files changed.
3. **Pushes** the current branch to `origin`.
4. **Dispatches** the GitHub Actions `release.yml` workflow with `tag=X.Y.Z`.

`src-tauri/tauri.conf.json` deliberately has no `version` field — Tauri 2 falls back to `Cargo.toml`. Don't add one back.

## Prerequisites

- `just`, `node`, `awk`, `git`, `gh` on PATH (all installed by `just setup`).
- `gh auth status` clean — the workflow dispatch needs a logged-in `gh`.
- Branch is the one you want to release from. The workflow checks out whatever ref it was dispatched against.
- Working tree is clean except for any pending bump. `just release` will commit the bump but won't stash unrelated changes.

## What the workflow does

`release.yml` runs the `Tauri bundles` matrix on linux-x86_64 / macos-aarch64 / macos-x86_64 / windows-x86_64. For each platform:

1. **Verify tag matches manifest versions** — fails fast if `Cargo.toml` or `package.json` disagree with the tag. If this trips, run `just release X.Y.Z` to fix.
2. Build the frontend with Vite, sanity-check `dist/` (entry rewritten, no SSR runtime leaked).
3. Build the Tauri bundle for the platform's target.
4. Package a portable binary (`anyai-<platform>.tar.gz` or `.zip`) with a SHA-256.
5. Upload bundle + portable to the GitHub release for the tag.

A separate `Upload installers` job attaches `scripts/install.sh` and `scripts/install.ps1`.

## Pre-release checklist

- `just check` is green locally.
- The version you're cutting is not already a published tag.
- `CHANGELOG`/release notes are ready (if applicable).
- Self-update sanity: the previous release's binary should detect the new one once it's published. The verify step protects against version mismatches that would break this.

## When something goes wrong

- **"version is X, expected Y"** in the verify step → run `just release Y` to bump and re-dispatch.
- **Workflow dispatch fails** → check `gh auth status` and that you're in the right repo.
- **Build fails on one platform only** → re-run the failed job from the Actions UI; the bump is already on `main`.
- **Wrong tag pushed** → delete the tag (`git tag -d vX.Y.Z && git push origin :refs/tags/vX.Y.Z`) and the GitHub release before re-running. Don't reuse a tag that already shipped binaries — self-update will get confused.

## Manual fallback

If `just release` is unavailable for some reason:

```bash
./scripts/bump-version.sh 0.1.8
git commit -am "chore(release): 0.1.8"
git push
gh workflow run release.yml -f tag=0.1.8
```

The same three files must end up with matching versions, or `release.yml` will reject the build.
