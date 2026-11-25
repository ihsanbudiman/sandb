#!/usr/bin/env bash
# Local release: build both macOS architectures on this Mac, sign them, write latest.json,
# then push the tag and publish the GitHub Release. Installed apps read latest.json to update.
# Run it after the "Release vX.Y.Z" commit (see docs/RELEASING.md).
set -euo pipefail
cd "$(dirname "$0")/.."

die() { echo "release: $*" >&2; exit 1; }
step() { printf '\n==> %s\n' "$*"; }

KEY_DIR="$HOME/.tauri"
IDENTITY="sandb Code Signing"
REPO="ihsanbudiman/sandb"

v=$(node -p "require('./package.json').version")
tag="v$v"

step "Checks for $tag"
[ -z "$(git status --porcelain --untracked-files=no)" ] || die "commit or stash the changes to tracked files first"
gh auth status >/dev/null 2>&1 || die "run 'gh auth login' first"
! gh release view "$tag" --repo "$REPO" >/dev/null 2>&1 || die "release $tag already exists on GitHub"
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  [ "$(git rev-list -n1 "$tag")" = "$(git rev-parse HEAD)" ] || die "tag $tag exists and does not point at HEAD"
fi
[ -f "$KEY_DIR/sandb.key" ] || die "updater key $KEY_DIR/sandb.key is missing"

# The CHANGELOG.md section of this version becomes the release notes on GitHub
# and in latest.json, which the app shows when it finds the update.
notes=$(awk -v v="$v" '
  /^## / { if (found) exit; if (index($0, "## [" v "]") == 1) { found = 1; next } }
  found
' CHANGELOG.md | sed '/./,$!d')
[ -n "$notes" ] || die "CHANGELOG.md has no section ## [$v]"

# Self-signed certificate. A fixed signature lets macOS keep the Keychain "Always Allow"
# permission across updates. Import it into the login keychain one time.
if ! security find-certificate -c "$IDENTITY" >/dev/null 2>&1; then
  step "Import code signing certificate into the login keychain (one time)"
  [ -f "$KEY_DIR/sandb-codesign.p12" ] || die "$KEY_DIR/sandb-codesign.p12 is missing"
  security import "$KEY_DIR/sandb-codesign.p12" -k "$HOME/Library/Keychains/login.keychain-db" \
    -P "$(cat "$KEY_DIR/sandb-codesign.p12.password")" -T /usr/bin/codesign
fi

rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
pnpm install --frozen-lockfile

export TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY_DIR/sandb.key")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""
export APPLE_SIGNING_IDENTITY="$IDENTITY"

out="src-tauri/target/release-upload/$tag"
rm -rf "$out"
mkdir -p "$out"

# target, updater arch suffix, latest.json platform
for spec in "aarch64-apple-darwin aarch64 darwin-aarch64" "x86_64-apple-darwin x64 darwin-x86_64"; do
  read -r target arch platform <<<"$spec"
  step "Build $target"
  pnpm tauri build --target "$target"
  bundle="src-tauri/target/$target/release/bundle"
  cp "$bundle/dmg/sandb_${v}_${arch}.dmg" "$out/"
  cp "$bundle/macos/sandb.app.tar.gz" "$out/sandb_${arch}.app.tar.gz"
  cp "$bundle/macos/sandb.app.tar.gz.sig" "$out/sandb_${arch}.app.tar.gz.sig"
done

step "Write latest.json"
base="https://github.com/$REPO/releases/download/$tag"
jq -n --arg version "$v" --arg notes "$notes" --arg date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg arm_sig "$(cat "$out/sandb_aarch64.app.tar.gz.sig")" --arg arm_url "$base/sandb_aarch64.app.tar.gz" \
  --arg x64_sig "$(cat "$out/sandb_x64.app.tar.gz.sig")" --arg x64_url "$base/sandb_x64.app.tar.gz" '
  { version: $version, notes: $notes, pub_date: $date,
    platforms: {
      "darwin-aarch64":     { signature: $arm_sig, url: $arm_url },
      "darwin-aarch64-app": { signature: $arm_sig, url: $arm_url },
      "darwin-x86_64":      { signature: $x64_sig, url: $x64_url },
      "darwin-x86_64-app":  { signature: $x64_sig, url: $x64_url } } }' >"$out/latest.json"

step "Push tag $tag"
git rev-parse -q --verify "refs/tags/$tag" >/dev/null || git tag "$tag"
git push origin "$tag"

step "Publish GitHub Release $tag"
gh release create "$tag" --repo "$REPO" --verify-tag --title "sandb $tag" --notes "$notes" "$out"/*

printf '\nPublished %s: https://github.com/%s/releases/tag/%s\n' "$tag" "$REPO" "$tag"
