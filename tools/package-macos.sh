#!/usr/bin/env bash
# Build OpenWalkieTalkie.app and its DMG (docs/host-service.md §10.1-§10.3).
#
#   tools/package-macos.sh [OUTPUT_DIRECTORY]      default: dist/
#
#   MACOS_SIGN_IDENTITY   a Developer ID Application identity. Unset (CI, a
#                         contributor without a certificate) packages an
#                         ad-hoc-signed app and says so; the identity
#                         assertion then reports "not a distributable build"
#                         instead of passing.
#   REQUIRE_SIGNING=1     a release build: refuse to finish without a
#                         Developer ID identity and a real Sparkle key.
#   MACOS_NOTARY_PROFILE  notarytool keychain profile (default OpenWalkieTalkie).
#                         Absent, notarisation prints the exact
#                         `store-credentials` line and SKIPS -- it never
#                         fakes a staple, and the manifest says `notarized:
#                         false` so an un-notarised DMG cannot look finished.
#   SPARKLE_PUBLIC_ED_KEY overrides packaging/macos/sparkle-public-key.txt.
#   OWT_MACOS_ARCH        arm64 (default, this Mac) or x86_64.
#   OWT_SPARKLE_ROOT      an existing Sparkle 2.9.6 tree, to build offline.
#   GITHUB_REPOSITORY     owner/name for the Sparkle feed URL.
#   OWT_RELEASE_README    optional README.md to include beside the app in DMG.
#
# This is openmicrokbd's scripts/package-macos.sh with ONE deliberate change
# (§10.1): every Mach-O in the bundle is signed
# `-i ai.cohdl.openwalkietalkie --entitlements packaging/macos/deck.entitlements`.
# The copied script has no `-i`, so `Contents/MacOS/deckd` would sign as the
# identifier `deckd` -- and TCC keys grants to the identifier, which would make
# the daemon's Microphone / Speech / Automation grant useless to the GUI and
# the GUI's useless to the daemon, with hardened-runtime capture from `deckd`
# failing even once a grant exists. tools/assert-bundle-identity.sh is the
# gate that proves it did not happen.
set -euo pipefail

case "${1:-}" in
    -h | --help) sed -n '2,31s/^# \{0,1\}//p' "$0"; exit 0 ;;
esac

root=$(cd "$(dirname "$0")/.." && pwd)
output_dir=${1:-$root/dist}
case "$output_dir" in
    '' | /) echo "package-macos: refusing unsafe output directory: $output_dir" >&2; exit 2 ;;
esac

identifier=ai.cohdl.openwalkietalkie
label=$identifier.deckd
packaging="$root/packaging/macos"
entitlements="$packaging/deck.entitlements"
sign_identity=${MACOS_SIGN_IDENTITY:-}
require_signing=${REQUIRE_SIGNING:-0}
notary_profile=${MACOS_NOTARY_PROFILE:-OpenWalkieTalkie}

requested_arch=${OWT_MACOS_ARCH:-$(uname -m)}
case "$requested_arch" in
    arm64 | aarch64) arch=aarch64; rust_target=aarch64-apple-darwin; lipo_arch=arm64 ;;
    x86_64) arch=x86_64; rust_target=x86_64-apple-darwin; lipo_arch=x86_64 ;;
    *) echo "package-macos: unsupported macOS architecture: $requested_arch" >&2; exit 2 ;;
esac

version=$(
    cargo metadata --manifest-path "$root/host/Cargo.toml" --locked --no-deps --format-version 1 |
        jq -r '.packages[] | select(.name == "openwalkietalkie-app") | .version'
)
[ -n "$version" ] || { echo "package-macos: could not read openwalkietalkie-app's version" >&2; exit 1; }
firmware_version=$(
    cargo metadata --manifest-path "$root/Cargo.toml" --locked --no-deps --format-version 1 |
        jq -r '.packages[] | select(.name == "openwalkietalkie-fw") | .version'
)

stage="$output_dir/macos-$arch"
app="$stage/OpenWalkieTalkie.app"
contents="$app/Contents"
resources="$contents/Resources"
frameworks="$contents/Frameworks"
agents="$contents/Library/LaunchAgents"
dmg_root="$stage/dmg-root"
dmg="$output_dir/OpenWalkieTalkie-$version-macos-$arch.dmg"
manifest="$output_dir/release-manifest.json"
sparkle_version=2.9.6
sparkle_root=${OWT_SPARKLE_ROOT:-$root/target/sparkle/$sparkle_version}
sparkle_repository=${GITHUB_REPOSITORY:-conol-ai/openwalkietalkie}
sparkle_feed_url="https://github.com/$sparkle_repository/releases/latest/download/appcast-$arch.xml"

say() { printf '%-4s  %s\n' "$1" "$2"; }

# --- the Sparkle Ed25519 public key --------------------------------------
# Sparkle refuses an update it cannot verify, so a bundle whose SUPublicEDKey
# is the checked-in placeholder simply cannot self-update. That is the honest
# failure, and it is fatal for a release build.
sparkle_public_key=${SPARKLE_PUBLIC_ED_KEY:-$(tr -d '\r\n' < "$packaging/sparkle-public-key.txt")}
if [[ "$sparkle_public_key" =~ ^[A-Za-z0-9+/]{43}=$ ]]; then
    sparkle_key_state=present
    sparkle_automatic_checks='<true/>'
else
    sparkle_key_state=placeholder
    sparkle_automatic_checks='<false/>'
    say warn "no Sparkle Ed25519 public key: $packaging/sparkle-public-key.txt"
    printf '      Updates are disabled in this build. To create the key pair:\n'
    printf '        %s/bin/generate_keys\n' "$sparkle_root"
    printf '      then put the printed public key in that file (the private half\n'
    printf '      stays in the login keychain and signs each appcast).\n'
    if [ "$require_signing" = 1 ]; then
        echo "package-macos: REQUIRE_SIGNING=1 needs a real Sparkle public key" >&2
        exit 1
    fi
fi
if [ -z "$sign_identity" ] && [ "$require_signing" = 1 ]; then
    echo "package-macos: REQUIRE_SIGNING=1 needs MACOS_SIGN_IDENTITY" >&2
    exit 1
fi

# --- build ----------------------------------------------------------------
rm -rf "$stage"
mkdir -p "$contents/MacOS" "$resources/licenses" "$frameworks" "$agents" "$dmg_root"

"$root/tools/fetch-sparkle.sh" "$sparkle_root" >/dev/null
say ok "Sparkle $sparkle_version ($sparkle_root)"

# The repo root's .cargo/config.toml targets the firmware's thumb triple, so
# the target and the target dir are always explicit (tools/host-test.sh).
OWT_SPARKLE_FRAMEWORK_DIR="$sparkle_root" MACOSX_DEPLOYMENT_TARGET=13.0 cargo build \
    --manifest-path "$root/host/Cargo.toml" \
    --release --locked \
    --target "$rust_target" --target-dir "$root/target/host" \
    -p deckd -p openwalkietalkie-app
built="$root/target/host/$rust_target/release"
say ok "built deckd + openwalkietalkie-app ($rust_target)"

# `deckd` goes in Contents/MacOS, NOT Contents/Resources: Apple DTS puts
# command-line helpers in MacOS, and a helper in Resources is sealed as a
# resource with no identity of its own. Do not "tidy" this.
install -m 755 "$built/openwalkietalkie-app" "$contents/MacOS/OpenWalkieTalkie"
install -m 755 "$built/deckd" "$contents/MacOS/deckd"
install -m 644 "$root/assets/branding/OpenWalkieTalkie.icns" "$resources/OpenWalkieTalkie.icns"
install -m 644 "$packaging/$label.plist" "$agents/$label.plist"
ditto "$sparkle_root/Sparkle.framework" "$frameworks/Sparkle.framework"
install -m 644 "$sparkle_root/LICENSE" "$resources/licenses/Sparkle-LICENSE.txt"
install -m 644 "$root/LICENSE" "$resources/licenses/OpenWalkieTalkie-LICENSE.txt"
install -m 644 "$root/host/crates/openwalkietalkie-app/assets/fonts/OFL.txt" "$resources/licenses/Geist-OFL.txt"
install -m 644 "$root/assets/fonts/OFL.txt" "$resources/licenses/NotoSans-OFL.txt"
install -m 644 "$root/assets/fonts/NotoSansMono-OFL.txt" "$resources/licenses/NotoSansMono-OFL.txt"
install -m 644 "$root/assets/lucide/LICENSE" "$resources/licenses/Lucide-LICENSE.txt"
install -m 644 "$root/site/public/fonts/PressStart2P-OFL.txt" "$resources/licenses/PressStart2P-OFL.txt"
# Each release carries both board images. The daemon verifies the manifest
# hash again before using a bundled image.
firmware_dir=${OWT_FIRMWARE_DIR:-$root/dist/firmware}
if [ -d "$firmware_dir" ]; then
    mkdir -p "$resources/firmware"
    for image in "$firmware_dir"/*.bin; do
        [ -f "$image" ] && install -m 644 "$image" "$resources/firmware/"
    done
    if [ -f "$firmware_dir/firmware-manifest.json" ]; then
        install -m 644 "$firmware_dir/firmware-manifest.json" "$resources/firmware/"
    fi
fi
if [ "$require_signing" = 1 ]; then
    [ -f "$resources/firmware/firmware-manifest.json" ] || {
        echo "package-macos: missing bundled firmware manifest" >&2; exit 1;
    }
    for board in hw-a hw-b; do
        for kind in "" "bootstrap-" "recovery-"; do
            [ -f "$resources/firmware/OpenWalkieTalkie-$firmware_version-${kind}$board.bin" ] || {
                echo "package-macos: missing ${kind}$board firmware" >&2; exit 1;
            }
        done
    done
fi
sparkle_enabled='<false/>'
if [ -n "$sign_identity" ] && [ "$sparkle_key_state" = present ]; then
    sparkle_enabled='<true/>'
fi
sed \
    -e "s|@SPARKLE_ENABLED@|$sparkle_enabled|g" \
    -e "s|@APP_VERSION@|$version|g" \
    -e "s|@SPARKLE_FEED_URL@|$sparkle_feed_url|g" \
    -e "s|@SPARKLE_PUBLIC_ED_KEY@|$sparkle_public_key|g" \
    -e "s|@SPARKLE_AUTOMATIC_CHECKS@|$sparkle_automatic_checks|g" \
    "$packaging/Info.plist.in" > "$contents/Info.plist"
printf 'APPL????' > "$contents/PkgInfo"

if grep -q '@[A-Z_]\{3,\}@' "$contents/Info.plist"; then
    echo "package-macos: unsubstituted placeholder left in Info.plist:" >&2
    grep -n '@[A-Z_]\{3,\}@' "$contents/Info.plist" >&2
    exit 1
fi
plutil -lint "$contents/Info.plist" >/dev/null
plutil -lint "$agents/$label.plist" >/dev/null
plutil -lint "$entitlements" >/dev/null
lipo "$contents/MacOS/OpenWalkieTalkie" -verify_arch "$lipo_arch"
lipo "$contents/MacOS/deckd" -verify_arch "$lipo_arch"
test -f "$frameworks/Sparkle.framework/Versions/B/Sparkle"
test -L "$frameworks/Sparkle.framework/Versions/Current"
say ok "staged $app"
otool -L "$contents/MacOS/OpenWalkieTalkie" | grep -q '@rpath/Sparkle.framework/' || {
    echo "package-macos: the app does not link the updater" >&2; exit 1;
}
say ok "OpenWalkieTalkie links Sparkle"

# --- sign, inside out -----------------------------------------------------
sparkle_framework="$frameworks/Sparkle.framework"
if [ -n "$sign_identity" ]; then
    signature_kind=developer-id
    sign=(codesign --force --options runtime --timestamp --sign "$sign_identity")
else
    # An ad-hoc signature has no timestamp service and no team, but it still
    # carries an identifier and entitlements, so the structure the assertion
    # checks is the same one a release build produces.
    signature_kind=ad-hoc
    sign=(codesign --force --options runtime --sign -)
    say warn "MACOS_SIGN_IDENTITY is unset; ad-hoc signing (not distributable)"
fi
# Sparkle's own parts keep Sparkle's identity and Sparkle's entitlements --
# our deck.entitlements belong to our code, not to its sandboxed XPC services.
"${sign[@]}" "$sparkle_framework/Versions/B/XPCServices/Installer.xpc"
"${sign[@]}" --preserve-metadata=entitlements \
    "$sparkle_framework/Versions/B/XPCServices/Downloader.xpc"
"${sign[@]}" "$sparkle_framework/Versions/B/Autoupdate"
"${sign[@]}" "$sparkle_framework/Versions/B/Updater.app"
"${sign[@]}" "$sparkle_framework"
# THE deliberate change: both Mach-Os, one identifier, one entitlements file.
# `deckd` first -- it is sealed into the app's CodeResources, so signing it
# after the bundle would break the seal.
"${sign[@]}" -i "$identifier" --entitlements "$entitlements" "$contents/MacOS/deckd"
"${sign[@]}" -i "$identifier" --entitlements "$entitlements" "$app"
codesign --verify --deep --strict "$sparkle_framework"
codesign --verify --deep --strict "$app"
say ok "signed ($signature_kind)"

# --- the gate -------------------------------------------------------------
printf '\n'
assertion=0
"$root/tools/assert-bundle-identity.sh" "$app" || assertion=$?
printf '\n'
case $assertion in
    0) distributable=true ;;
    3)
        distributable=false
        if [ "$require_signing" = 1 ]; then
            echo "package-macos: REQUIRE_SIGNING=1 and the bundle is only ad-hoc signed" >&2
            exit 1
        fi
        ;;
    *) echo "package-macos: the identity assertion failed; not packaging a DMG" >&2; exit 1 ;;
esac

# --- DMG ------------------------------------------------------------------
rm -rf "$dmg_root"
mkdir -p "$dmg_root"
ditto "$app" "$dmg_root/OpenWalkieTalkie.app"
ln -s /Applications "$dmg_root/Applications"
if [ -n "${OWT_RELEASE_README:-}" ]; then
    install -m 644 "$OWT_RELEASE_README" "$dmg_root/README.md"
fi
rm -f "$dmg"
hdiutil create -volname OpenWalkieTalkie -srcfolder "$dmg_root" -format UDZO -ov "$dmg" >/dev/null
if [ -n "$sign_identity" ]; then
    codesign --force --timestamp --sign "$sign_identity" "$dmg"
    codesign --verify --strict "$dmg"
fi
hdiutil verify "$dmg" >/dev/null
dmg_sha256=$(shasum -a 256 "$dmg" | awk '{print $1}')
say ok "DMG $(basename "$dmg") ($dmg_sha256)"

# --- notarise, or skip loudly (never fake a staple) -----------------------
team=$(codesign -dv "$contents/MacOS/deckd" 2>&1 | sed -n 's/^TeamIdentifier=//p')
notarized=false
if [ "$distributable" != true ]; then
    notarization="skipped: $signature_kind build; notarisation requires a Developer ID signature"
elif ! xcrun notarytool history --keychain-profile "$notary_profile" >/dev/null 2>&1; then
    notarization="skipped: no notarytool keychain profile '$notary_profile' on this machine"
else
    say ".." "notarising $(basename "$dmg") (profile $notary_profile)"
    if xcrun notarytool submit "$dmg" --keychain-profile "$notary_profile" --wait &&
       bash "$root/tools/staple-macos.sh" "$dmg"; then
        notarized=true
        notarization="notarised and stapled with keychain profile '$notary_profile'"
        dmg_sha256=$(shasum -a 256 "$dmg" | awk '{print $1}')
    else
        notarization="FAILED with keychain profile '$notary_profile'"
        say FAIL "$notarization"
    fi
fi
if [ "$notarized" != true ]; then
    say skip "notarisation $notarization"
    if [ "$distributable" = true ]; then
        printf '      Only the operator can create the credential. Either:\n'
        printf '        xcrun notarytool store-credentials %s \\\n' "$notary_profile"
        printf '            --apple-id <your-apple-id> \\\n'
        printf '            --team-id %s \\\n' "${team:-<TEAMID>}"
        printf '            --password <app-specific password from appleid.apple.com>\n'
        printf '      or, with an App Store Connect key:\n'
        printf '        xcrun notarytool store-credentials %s \\\n' "$notary_profile"
        printf '            --key <AuthKey_XXXXXXXX.p8> --key-id <KEYID> --issuer <ISSUER-UUID>\n'
        printf '      Until then this DMG is signed but NOT notarised: Gatekeeper\n'
        printf '      on another Mac will refuse it, and the manifest says so.\n'
    fi
fi

# --- manifest -------------------------------------------------------------
# An un-notarised DMG must say so here rather than look finished.
jq -n \
    --arg product OpenWalkieTalkie \
    --arg version "$version" \
    --arg arch "$arch" \
    --arg identifier "$identifier" \
    --arg team "${team:-}" \
    --arg signature "$signature_kind" \
    --arg sign_identity "$sign_identity" \
    --arg sparkle_version "$sparkle_version" \
    --arg sparkle_feed "$sparkle_feed_url" \
    --arg sparkle_key "$sparkle_key_state" \
    --arg dmg "$(basename "$dmg")" \
    --arg dmg_sha256 "$dmg_sha256" \
    --argjson dmg_bytes "$(stat -f %z "$dmg")" \
    --argjson notarized "$notarized" \
    --arg notarization "$notarization" \
    --argjson distributable "$distributable" \
    --arg built_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{
        product: $product, version: $version, arch: $arch, built_at: $built_at,
        bundle: {
            identifier: $identifier,
            team_identifier: (if $team == "" then null else $team end),
            signature: $signature,
            signing_identity: (if $sign_identity == "" then null else $sign_identity end),
            hardened_runtime: true,
            entitlements: [
                "com.apple.security.device.audio-input"
            ]
        },
        sparkle: {
            framework_version: $sparkle_version,
            feed_url: $sparkle_feed,
            public_ed_key: $sparkle_key,
            appcast_signed: false
        },
        dmg: { name: $dmg, sha256: $dmg_sha256, bytes: $dmg_bytes },
        notarized: $notarized,
        notarization: $notarization,
        distributable: ($distributable and $notarized)
    }' > "$manifest"
say ok "manifest $manifest"

printf '\n'
printf 'app:      %s\n' "$app"
printf 'dmg:      %s\n' "$dmg"
printf 'sha256:   %s\n' "$dmg_sha256"
printf 'signature: %s   notarised: %s\n' "$signature_kind" "$notarized"

if [ "$require_signing" = 1 ] && [ "$notarized" != true ]; then
    echo "package-macos: release requires successful notarization" >&2
    exit 1
fi
