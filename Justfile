set windows-shell := ["sh", "-cu"]

app_name  := "Petit Mates"
exe_name  := "petitmates"
bundle_id := "jp.emotiongraphics.petitmates"
min_macos := "13.0"
version   := `awk -F'"' '/^version *=/{print $2; exit}' Cargo.toml`
pkg_name  := replace(app_name, " ", "-")

rust_target_arm64 := "aarch64-apple-darwin"
rust_target_x86   := "x86_64-apple-darwin"

settings_dir    := env_var_or_default("SETTINGS_DIR", "../settings")
settings_schema := justfile_directory() + "/schema.toml"
win_target_dir  := "/tmp/pm-win"

default: help

help:
    @just --list

dev: darwin-build-arm64 win-build

release: darwin-release win-release

# ---------------------------------------------------------------------------
# macOS
# ---------------------------------------------------------------------------

darwin-build: darwin-build-arm64 darwin-build-x86_64

darwin-build-arm64: _settings-arm64
    just _darwin-bundle darwin-arm64 {{rust_target_arm64}}
    @echo "App bundle: dist/darwin-arm64/{{app_name}}.app"

darwin-build-x86_64: _settings-x86_64
    just _darwin-bundle darwin-x86_64 {{rust_target_x86}}
    @echo "App bundle: dist/darwin-x86_64/{{app_name}}.app"

darwin-sign-arm64: darwin-build-arm64
    just _require-cert
    xattr -cr "dist/darwin-arm64/{{app_name}}.app"
    codesign --deep --force --options runtime \
        --entitlements assets/darwin/entitlements.plist \
        --sign "${APPLE_DEVELOPER_CERTIFICATE_NAME}" \
        "dist/darwin-arm64/{{app_name}}.app"
    @echo "Signed: dist/darwin-arm64/{{app_name}}.app"

darwin-sign-x86_64: darwin-build-x86_64
    just _require-cert
    xattr -cr "dist/darwin-x86_64/{{app_name}}.app"
    codesign --deep --force --options runtime \
        --entitlements assets/darwin/entitlements.plist \
        --sign "${APPLE_DEVELOPER_CERTIFICATE_NAME}" \
        "dist/darwin-x86_64/{{app_name}}.app"
    @echo "Signed: dist/darwin-x86_64/{{app_name}}.app"

darwin-notarize-arm64: darwin-sign-arm64
    #!/usr/bin/env bash
    set -euo pipefail
    just _require-notarize-env
    just _require-dmgbuild
    dmgbuild -s assets/darwin/dmg_settings.py \
        -D app="dist/darwin-arm64/{{app_name}}.app" \
        "{{app_name}}" "dist/{{pkg_name}}-v{{version}}-darwin-arm64.dmg"
    xcrun notarytool submit "dist/{{pkg_name}}-v{{version}}-darwin-arm64.dmg" \
        --apple-id  "${APPLE_ID}" \
        --password  "${APPLE_DEVELOPER_APP_PASSWORD}" \
        --team-id   "${APPLE_DEVELOPER_TEAM_ID}" \
        --wait
    xcrun stapler staple "dist/{{pkg_name}}-v{{version}}-darwin-arm64.dmg"
    echo "Notarized and stapled: dist/{{pkg_name}}-v{{version}}-darwin-arm64.dmg"

darwin-notarize-x86_64: darwin-sign-x86_64
    #!/usr/bin/env bash
    set -euo pipefail
    just _require-notarize-env
    just _require-dmgbuild
    dmgbuild -s assets/darwin/dmg_settings.py \
        -D app="dist/darwin-x86_64/{{app_name}}.app" \
        "{{app_name}}" "dist/{{pkg_name}}-v{{version}}-darwin-x86_64.dmg"
    xcrun notarytool submit "dist/{{pkg_name}}-v{{version}}-darwin-x86_64.dmg" \
        --apple-id  "${APPLE_ID}" \
        --password  "${APPLE_DEVELOPER_APP_PASSWORD}" \
        --team-id   "${APPLE_DEVELOPER_TEAM_ID}" \
        --wait
    xcrun stapler staple "dist/{{pkg_name}}-v{{version}}-darwin-x86_64.dmg"
    echo "Notarized and stapled: dist/{{pkg_name}}-v{{version}}-darwin-x86_64.dmg"

# Unsigned DMG for layout testing only (Gatekeeper blocks unsigned apps).
darwin-dmg-arm64: darwin-build-arm64
    just _require-dmgbuild
    dmgbuild -s assets/darwin/dmg_settings.py \
        -D app="dist/darwin-arm64/{{app_name}}.app" \
        "{{app_name}}" "dist/{{pkg_name}}-v{{version}}-darwin-arm64.dmg"
    @echo "Package: dist/{{pkg_name}}-v{{version}}-darwin-arm64.dmg"

darwin-dmg-x86_64: darwin-build-x86_64
    just _require-dmgbuild
    dmgbuild -s assets/darwin/dmg_settings.py \
        -D app="dist/darwin-x86_64/{{app_name}}.app" \
        "{{app_name}}" "dist/{{pkg_name}}-v{{version}}-darwin-x86_64.dmg"
    @echo "Package: dist/{{pkg_name}}-v{{version}}-darwin-x86_64.dmg"

# Sparkle zip — must be built from a notarized and stapled .app.
darwin-zip-arm64: darwin-notarize-arm64
    ditto -c -k --keepParent \
        "dist/darwin-arm64/{{app_name}}.app" \
        "dist/{{pkg_name}}-v{{version}}-darwin-arm64.zip"
    @echo "Package: dist/{{pkg_name}}-v{{version}}-darwin-arm64.zip"

darwin-zip-x86_64: darwin-notarize-x86_64
    ditto -c -k --keepParent \
        "dist/darwin-x86_64/{{app_name}}.app" \
        "dist/{{pkg_name}}-v{{version}}-darwin-x86_64.zip"
    @echo "Package: dist/{{pkg_name}}-v{{version}}-darwin-x86_64.zip"

darwin-release: darwin-notarize-arm64 darwin-notarize-x86_64

[macos]
install: darwin-build-arm64
    rm -rf "/Applications/{{app_name}}.app"
    cp -r "dist/darwin-arm64/{{app_name}}.app" "/Applications/"

# ---------------------------------------------------------------------------
# Windows
# ---------------------------------------------------------------------------

win-build: _settings-win
    CARGO_TARGET_DIR="{{win_target_dir}}" cargo build --release --target x86_64-pc-windows-gnu
    mkdir -p "dist/windows-x86_64"
    cp "{{win_target_dir}}/x86_64-pc-windows-gnu/release/{{exe_name}}.exe" \
        "dist/windows-x86_64/{{app_name}}.exe"
    cp "{{settings_dir}}/dist/settings/windows-x86_64/Settings.exe" \
        "dist/windows-x86_64/Settings.exe"
    @echo "Windows build: dist/windows-x86_64"

win-zip: win-build
    #!/usr/bin/env bash
    set -euo pipefail
    ZIP="dist/{{pkg_name}}-v{{version}}-windows-x86_64.zip"
    rm -f "${ZIP}"
    cd "dist/windows-x86_64" && zip "../$(basename "${ZIP}")" "{{app_name}}.exe" "Settings.exe"
    echo "Package: ${ZIP}"

win-release: win-zip

# ---------------------------------------------------------------------------
# Utilities
# ---------------------------------------------------------------------------

clean:
    rm -rf dist

# ---------------------------------------------------------------------------
# Internal
# ---------------------------------------------------------------------------

_settings-arm64:
    SETTINGS_SCHEMA="{{settings_schema}}" \
        just --justfile "{{settings_dir}}/Justfile" binary-arm64

_settings-x86_64:
    SETTINGS_SCHEMA="{{settings_schema}}" \
        just --justfile "{{settings_dir}}/Justfile" binary-x86_64

_settings-win:
    SETTINGS_SCHEMA="{{settings_schema}}" \
        just --justfile "{{settings_dir}}/Justfile" settings-win-build

_darwin-bundle arch rust_target:
    MACOSX_DEPLOYMENT_TARGET={{min_macos}} cargo build --release --target {{rust_target}}
    mkdir -p "dist/{{arch}}/{{app_name}}.app/Contents/MacOS"
    mkdir -p "dist/{{arch}}/{{app_name}}.app/Contents/Resources"
    cp "target/{{rust_target}}/release/{{exe_name}}" \
        "dist/{{arch}}/{{app_name}}.app/Contents/MacOS/{{exe_name}}"
    cp "{{settings_dir}}/target/{{rust_target}}/release/settings" \
        "dist/{{arch}}/{{app_name}}.app/Contents/MacOS/settings"
    just _copy-assets "dist/{{arch}}/{{app_name}}.app/Contents/Resources"
    just _plist "dist/{{arch}}/{{app_name}}.app/Contents"
    just _icns "dist/{{arch}}/{{app_name}}.app/Contents/Resources/AppIcon.icns"

_copy-assets res_dir:
    mkdir -p "{{res_dir}}/assets/bearded_dragon/sprite"
    cp assets/bearded_dragon/manifest.toml  "{{res_dir}}/assets/bearded_dragon/"
    cp assets/bearded_dragon/sprite/*.png   "{{res_dir}}/assets/bearded_dragon/sprite/"
    mkdir -p "{{res_dir}}/assets/pond_turtle/sprite"
    cp assets/pond_turtle/manifest.toml     "{{res_dir}}/assets/pond_turtle/"
    cp assets/pond_turtle/sprite/*.png      "{{res_dir}}/assets/pond_turtle/sprite/"
    mkdir -p "{{res_dir}}/assets/leopard_gecko/sprite"
    cp assets/leopard_gecko/manifest.toml   "{{res_dir}}/assets/leopard_gecko/"
    cp assets/leopard_gecko/sprite/*.png    "{{res_dir}}/assets/leopard_gecko/sprite/"
    mkdir -p "{{res_dir}}/assets/common"
    cp assets/common/params.toml            "{{res_dir}}/assets/common/"

_plist contents_dir:
    sed 's/@VERSION@/{{version}}/g' assets/darwin/Info.plist > "{{contents_dir}}/Info.plist"

_icns icns_out:
    #!/usr/bin/env bash
    if [ ! -f "assets/appicon.png" ]; then
        echo "Note: assets/appicon.png not found — skipping icon generation."
        exit 0
    fi
    set -euo pipefail
    ICONSET_WORK="$(mktemp -d)"
    ICONSET="${ICONSET_WORK}/AppIcon.iconset"
    mkdir -p "${ICONSET}"
    SRC_NORM="${ICONSET_WORK}/source-1024.png"
    sips -z 1024 1024 "assets/appicon.png" --out "${SRC_NORM}" >/dev/null
    sips --deleteColorManagementProperties "${SRC_NORM}" >/dev/null 2>&1 || true
    sips -z 16   16   "${SRC_NORM}" --out "${ICONSET}/icon_16x16.png"
    sips -z 32   32   "${SRC_NORM}" --out "${ICONSET}/icon_16x16@2x.png"
    sips -z 32   32   "${SRC_NORM}" --out "${ICONSET}/icon_32x32.png"
    sips -z 64   64   "${SRC_NORM}" --out "${ICONSET}/icon_32x32@2x.png"
    sips -z 128  128  "${SRC_NORM}" --out "${ICONSET}/icon_128x128.png"
    sips -z 256  256  "${SRC_NORM}" --out "${ICONSET}/icon_128x128@2x.png"
    sips -z 256  256  "${SRC_NORM}" --out "${ICONSET}/icon_256x256.png"
    sips -z 512  512  "${SRC_NORM}" --out "${ICONSET}/icon_256x256@2x.png"
    sips -z 512  512  "${SRC_NORM}" --out "${ICONSET}/icon_512x512.png"
    cp "${SRC_NORM}" "${ICONSET}/icon_512x512@2x.png"
    mkdir -p "$(dirname "{{icns_out}}")"
    ICNS_OUT_ABS="$(cd "$(dirname "{{icns_out}}")" && pwd)/$(basename "{{icns_out}}")"
    iconutil -c icns "${ICONSET}" -o "${ICNS_OUT_ABS}"
    rm -rf "${ICONSET_WORK}"

_require-dmgbuild:
    #!/usr/bin/env bash
    command -v dmgbuild >/dev/null 2>&1 || \
        { echo "Error: dmgbuild not found. Run: pipx install dmgbuild" >&2; exit 1; }

_require-cert:
    #!/usr/bin/env bash
    test -n "${APPLE_DEVELOPER_CERTIFICATE_NAME:-}" || \
        { echo "Error: APPLE_DEVELOPER_CERTIFICATE_NAME is not set" >&2; exit 1; }

_require-notarize-env:
    #!/usr/bin/env bash
    test -n "${APPLE_DEVELOPER_TEAM_ID:-}" || \
        { echo "Error: APPLE_DEVELOPER_TEAM_ID is not set" >&2; exit 1; }
    test -n "${APPLE_ID:-}" || \
        { echo "Error: APPLE_ID is not set" >&2; exit 1; }
    test -n "${APPLE_DEVELOPER_APP_PASSWORD:-}" || \
        { echo "Error: APPLE_DEVELOPER_APP_PASSWORD is not set" >&2; exit 1; }
