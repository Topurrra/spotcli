#!/usr/bin/env bash
# Cut a release from this machine: bump the version, run the checks, build
# the Windows package, tag, push, and publish it on GitHub.
#
#   scripts/release.sh 1.1.0
#
# Needs: a clean tree on main, and the GitHub CLI (gh) signed in.
set -euo pipefail
cd "$(dirname "$0")/.."

version="${1:?usage: scripts/release.sh <version, e.g. 1.1.0>}"
version="${version#v}"
tag="v$version"

gh="$(command -v gh || echo "/c/Program Files/GitHub CLI/gh.exe")"
[ -x "$gh" ] || { echo "GitHub CLI (gh) not found"; exit 1; }

[ "$(git branch --show-current)" = main ] || { echo "switch to main first"; exit 1; }
[ -z "$(git status --porcelain)" ] || { echo "commit or stash your changes first"; exit 1; }
git rev-parse -q --verify "refs/tags/$tag" >/dev/null && { echo "$tag already exists"; exit 1; }

echo "==> version $version"
sed -i "0,/^version = \".*\"/s//version = \"$version\"/" Cargo.toml

scripts/check.sh

echo "==> release build"
cargo build --release --quiet
built="$(target/release/spotcli --version)"
[ "$built" = "spotcli $version" ] || { echo "unexpected version: $built"; exit 1; }

echo "==> package"
name="spotcli-$tag-x86_64-windows"
out="target/package"
rm -rf "$out" && mkdir -p "$out/$name"
cp target/release/spotcli.exe packaging/windows/install.ps1 README.md LICENSE "$out/$name/"
powershell.exe -NoProfile -Command \
  "Compress-Archive -Path '$out/$name' -DestinationPath '$out/$name.zip' -Force"

echo "==> commit, tag, push"
git add Cargo.toml Cargo.lock
git commit --quiet -m "Release $version"
git tag -a "$tag" -m "spotcli $version"
git push --quiet origin main "$tag"

echo "==> publish"
"$gh" release create "$tag" "$out/$name.zip" --title "spotcli $tag" --generate-notes --latest
echo "==> done: $("$gh" release view "$tag" --json url -q .url)"
