#!/usr/bin/env bash
# Bump the release version in every file that pins it.
#
# src-tauri/Cargo.toml is the single source of truth for the app version.
# tauri.conf.json deliberately omits "version" — Tauri 2 falls back to
# Cargo.toml. package.json + src-tauri/Cargo.lock are kept in sync here so
# that downstream tooling and the release.yml verify step agree.
#
# Usage: scripts/bump-version.sh 0.1.8
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <version>" >&2
  exit 64
fi

v="${1#v}"
if ! [[ "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "error: '$v' is not a semver version (expected X.Y.Z)" >&2
  exit 64
fi

cd "$(git rev-parse --show-toplevel)"

# src-tauri/Cargo.toml — bump the [package] version (the first `version = "..."`).
awk -v v="$v" '
  /^\[package\]/ { in_pkg=1 }
  /^\[/ && !/^\[package\]/ { in_pkg=0 }
  in_pkg && /^version = "[^"]*"/ && !done { print "version = \"" v "\""; done=1; next }
  { print }
' src-tauri/Cargo.toml > src-tauri/Cargo.toml.tmp
mv src-tauri/Cargo.toml.tmp src-tauri/Cargo.toml

# src-tauri/Cargo.lock — bump the [[package]] entry whose name is "anyai".
awk -v v="$v" '
  /^\[\[package\]\]/ { hit=0 }
  /^name = "anyai"$/ { hit=1 }
  hit && /^version = "[^"]*"/ { print "version = \"" v "\""; hit=0; next }
  { print }
' src-tauri/Cargo.lock > src-tauri/Cargo.lock.tmp
mv src-tauri/Cargo.lock.tmp src-tauri/Cargo.lock

# package.json — node is the most portable JSON editor we can rely on here.
node -e '
  const fs = require("fs");
  const f = "package.json";
  const j = JSON.parse(fs.readFileSync(f, "utf8"));
  j.version = process.argv[1];
  fs.writeFileSync(f, JSON.stringify(j, null, 2) + "\n");
' "$v"

echo "Bumped Cargo.toml, Cargo.lock, and package.json to $v."
