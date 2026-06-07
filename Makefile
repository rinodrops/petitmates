empty     :=
space     := $(empty) $(empty)

APP_NAME  := Petit Mates
EXE_NAME  := petitmates
BUNDLE_ID := jp.emotiongraphics.petitmates
VERSION   := $(shell awk -F'"' '/^version *=/{print $$2; exit}' Cargo.toml)
MIN_MACOS := 13.0

# Distribution layout:
#   dist/darwin-arm64/Petit Mates.app
#   dist/darwin-x86_64/Petit Mates.app
#   dist/windows-x86_64/Petit Mates.exe + Settings.exe
#   dist/Petit-Mates-v$(VERSION)-darwin-*.dmg / -windows-x86_64.zip  (packages at dist/ root)
ARCH_DARWIN_ARM64 := darwin-arm64
ARCH_DARWIN_X86   := darwin-x86_64
ARCH_WIN          := windows-x86_64

RUST_TARGET_ARM64 := aarch64-apple-darwin
RUST_TARGET_X86   := x86_64-apple-darwin

DIST_DIR := dist

APP_ARM64 := $(DIST_DIR)/$(ARCH_DARWIN_ARM64)/$(APP_NAME).app
APP_X86   := $(DIST_DIR)/$(ARCH_DARWIN_X86)/$(APP_NAME).app

APP_DMG_ARM64 := $(DIST_DIR)/Petit-Mates-v$(VERSION)-darwin-arm64.dmg
APP_DMG_X86   := $(DIST_DIR)/Petit-Mates-v$(VERSION)-darwin-x86_64.dmg
APP_ZIP_ARM64 := $(DIST_DIR)/Petit-Mates-v$(VERSION)-darwin-arm64.zip
APP_ZIP_X86   := $(DIST_DIR)/Petit-Mates-v$(VERSION)-darwin-x86_64.zip

DMG_SETTINGS := dmg_settings.py

WIN_DIR      := $(DIST_DIR)/$(ARCH_WIN)
WIN_EXE_NAME := Petit Mates
WIN_EXE      := $(WIN_DIR)/$(WIN_EXE_NAME).exe
WIN_SETTINGS_EXE := Settings.exe
WIN_ZIP      := $(DIST_DIR)/Petit-Mates-v$(VERSION)-windows-x86_64.zip
WIN_TARGET_DIR := /tmp/pm-win

BD_SRC   := assets/bearded_dragon
PT_SRC   := assets/pond_turtle
LG_SRC   := assets/leopard_gecko
ICON_SRC := assets/appicon.png
ICONSET  := $(DIST_DIR)/AppIcon.iconset

CERT      := $(APPLE_DEVELOPER_CERTIFICATE_NAME)
TEAM_ID   := $(APPLE_DEVELOPER_TEAM_ID)
APPLE_ID_ := $(APPLE_ID)
APP_PASS  := $(APPLE_DEVELOPER_APP_PASSWORD)

# Settings UI (local dev: repos/settings as ../settings; CI: checkout at ./settings)
SETTINGS_DIR        ?= ../settings
SETTINGS_SCHEMA     := $(abspath schema.toml)
SETTINGS_BIN_ARM64  := $(SETTINGS_DIR)/target/$(RUST_TARGET_ARM64)/release/settings
SETTINGS_BIN_X86    := $(SETTINGS_DIR)/target/$(RUST_TARGET_X86)/release/settings
SETTINGS_WIN_EXE    := $(SETTINGS_DIR)/dist/settings/windows-x86_64/Settings.exe

.PHONY: all dev app app-arm64 app-x86_64 win win-zip \
	mac-dmg-arm64 mac-dmg-x86_64 mac-zip-arm64 mac-zip-x86_64 \
	sign-arm64 sign-x86_64 notarize-arm64 notarize-x86_64 mac-release \
	settings-arm64 settings-x86_64 settings-win \
	_mac_app _copy_assets _plist _icns_if_present _icns_build clean help

all: mac-release win-zip

# Daily development: arm64 .app + Windows folder (unsigned).
dev: app-arm64 win

# Unsigned macOS .app bundles for both architectures (local testing).
app: app-arm64 app-x86_64

# -----------------------------------------------------------------------
# macOS .app bundles (per architecture, no lipo)
# -----------------------------------------------------------------------

app-arm64: settings-arm64
	$(MAKE) _mac_app PM_ARCH_SLUG=$(ARCH_DARWIN_ARM64) \
		PM_RUST_TARGET=$(RUST_TARGET_ARM64) \
		PM_SETTINGS_BIN=$(SETTINGS_BIN_ARM64)
	@echo "App bundle: $(APP_ARM64)"

app-x86_64: settings-x86_64
	$(MAKE) _mac_app PM_ARCH_SLUG=$(ARCH_DARWIN_X86) \
		PM_RUST_TARGET=$(RUST_TARGET_X86) \
		PM_SETTINGS_BIN=$(SETTINGS_BIN_X86)
	@echo "App bundle: $(APP_X86)"

# Internal: build petitmates + bundle assets into dist/$(PM_ARCH_SLUG)/Petit Mates.app
# Requires: PM_ARCH_SLUG PM_RUST_TARGET PM_SETTINGS_BIN
.PHONY: _mac_app
_mac_app:
	@test -n "$(PM_ARCH_SLUG)" && test -n "$(PM_RUST_TARGET)" && test -n "$(PM_SETTINGS_BIN)"
	MACOSX_DEPLOYMENT_TARGET=$(MIN_MACOS) cargo build --release --target $(PM_RUST_TARGET)
	@mkdir -p "$(DIST_DIR)/$(PM_ARCH_SLUG)/$(APP_NAME).app/Contents/MacOS"
	@mkdir -p "$(DIST_DIR)/$(PM_ARCH_SLUG)/$(APP_NAME).app/Contents/Resources"
	cp target/$(PM_RUST_TARGET)/release/$(EXE_NAME) \
		"$(DIST_DIR)/$(PM_ARCH_SLUG)/$(APP_NAME).app/Contents/MacOS/$(EXE_NAME)"
	cp "$(PM_SETTINGS_BIN)" \
		"$(DIST_DIR)/$(PM_ARCH_SLUG)/$(APP_NAME).app/Contents/MacOS/settings"
	$(MAKE) _copy_assets RES_DIR="$(DIST_DIR)/$(PM_ARCH_SLUG)/$(APP_NAME).app/Contents/Resources"
	$(MAKE) _plist PLIST_CONTENTS="$(DIST_DIR)/$(PM_ARCH_SLUG)/$(APP_NAME).app/Contents"
	$(MAKE) _icns_if_present ICNS_RES_DIR="$(DIST_DIR)/$(PM_ARCH_SLUG)/$(APP_NAME).app/Contents/Resources" \
		ICNS_OUT="$(DIST_DIR)/$(PM_ARCH_SLUG)/$(APP_NAME).app/Contents/Resources/AppIcon.icns"

.PHONY: _copy_assets
_copy_assets:
	@test -n "$(RES_DIR)"
	mkdir -p "$(RES_DIR)/assets/bearded_dragon/sprite"
	cp $(BD_SRC)/manifest.toml   "$(RES_DIR)/assets/bearded_dragon/"
	cp $(BD_SRC)/sprite/*.png    "$(RES_DIR)/assets/bearded_dragon/sprite/"
	mkdir -p "$(RES_DIR)/assets/pond_turtle/sprite"
	cp $(PT_SRC)/manifest.toml   "$(RES_DIR)/assets/pond_turtle/"
	cp $(PT_SRC)/sprite/*.png    "$(RES_DIR)/assets/pond_turtle/sprite/"
	mkdir -p "$(RES_DIR)/assets/leopard_gecko/sprite"
	cp $(LG_SRC)/manifest.toml   "$(RES_DIR)/assets/leopard_gecko/"
	cp $(LG_SRC)/sprite/*.png    "$(RES_DIR)/assets/leopard_gecko/sprite/"
	mkdir -p "$(RES_DIR)/assets/common"
	cp assets/common/params.toml "$(RES_DIR)/assets/common/"

# -----------------------------------------------------------------------
# Settings (matched to each macOS Rust target)
# -----------------------------------------------------------------------

settings-arm64:
	@test -f '$(SETTINGS_DIR)/assets/icons.ttf' || $(MAKE) -C '$(SETTINGS_DIR)' icons
	cd '$(SETTINGS_DIR)' && SETTINGS_SCHEMA='$(SETTINGS_SCHEMA)' \
		MACOSX_DEPLOYMENT_TARGET=$(MIN_MACOS) \
		cargo build --release --target $(RUST_TARGET_ARM64)

settings-x86_64:
	@test -f '$(SETTINGS_DIR)/assets/icons.ttf' || $(MAKE) -C '$(SETTINGS_DIR)' icons
	cd '$(SETTINGS_DIR)' && SETTINGS_SCHEMA='$(SETTINGS_SCHEMA)' \
		MACOSX_DEPLOYMENT_TARGET=$(MIN_MACOS) \
		cargo build --release --target $(RUST_TARGET_X86)

# Inline build: settings Makefile passes SETTINGS_SCHEMA unquoted (breaks paths with spaces).
settings-win:
	@test -f '$(SETTINGS_DIR)/assets/icons.ttf' || $(MAKE) -C '$(SETTINGS_DIR)' icons
	@test -f '$(SETTINGS_DIR)/assets/appicon.ico' || $(MAKE) -C '$(SETTINGS_DIR)' appicon-ico
	cd '$(SETTINGS_DIR)' && SETTINGS_SCHEMA='$(SETTINGS_SCHEMA)' CARGO_TARGET_DIR=/tmp/settings-win \
		cargo build --release -p settings --target x86_64-pc-windows-gnu
	@mkdir -p '$(SETTINGS_DIR)/dist/settings/windows-x86_64'
	cp /tmp/settings-win/x86_64-pc-windows-gnu/release/settings.exe '$(SETTINGS_WIN_EXE)'

# -----------------------------------------------------------------------
# Windows cross-compile (x86_64, from macOS)
# Requires: mingw-w64 (brew install mingw-w64)
# Uses a space-free CARGO_TARGET_DIR to work around dlltool limitation.
# -----------------------------------------------------------------------

win: settings-win
	CARGO_TARGET_DIR="$(WIN_TARGET_DIR)" cargo build --release --target x86_64-pc-windows-gnu
	mkdir -p "$(WIN_DIR)"
	cp "$(WIN_TARGET_DIR)/x86_64-pc-windows-gnu/release/$(EXE_NAME).exe" "$(WIN_EXE)"
	cp "$(SETTINGS_WIN_EXE)" "$(WIN_DIR)/$(WIN_SETTINGS_EXE)"
	@echo "Windows build: $(WIN_DIR)"

win-zip: win
	rm -f "$(WIN_ZIP)"
	cd "$(WIN_DIR)" && zip "../$(notdir $(WIN_ZIP))" "$(WIN_EXE_NAME).exe" "$(WIN_SETTINGS_EXE)"
	@echo "Windows package: $(WIN_ZIP)"

# -----------------------------------------------------------------------
# macOS packages
# -----------------------------------------------------------------------

mac-dmg-arm64: app-arm64
	@command -v dmgbuild >/dev/null 2>&1 || (echo "Error: dmgbuild not found. Run: pipx install dmgbuild" && exit 1)
	dmgbuild -s "$(DMG_SETTINGS)" -D app="$(APP_ARM64)" "$(APP_NAME)" "$(APP_DMG_ARM64)"
	@echo "Package: $(APP_DMG_ARM64)"

mac-dmg-x86_64: app-x86_64
	@command -v dmgbuild >/dev/null 2>&1 || (echo "Error: dmgbuild not found. Run: pipx install dmgbuild" && exit 1)
	dmgbuild -s "$(DMG_SETTINGS)" -D app="$(APP_X86)" "$(APP_NAME)" "$(APP_DMG_X86)"
	@echo "Package: $(APP_DMG_X86)"

# Sparkle / auto-update: signed .app zip (not part of `all` or `mac-release`).
mac-zip-arm64: app-arm64
	ditto -c -k --keepParent "$(APP_ARM64)" "$(APP_ZIP_ARM64)"
	@echo "Package: $(APP_ZIP_ARM64)"

mac-zip-x86_64: app-x86_64
	ditto -c -k --keepParent "$(APP_X86)" "$(APP_ZIP_X86)"
	@echo "Package: $(APP_ZIP_X86)"

# -----------------------------------------------------------------------
# Code signing (per architecture)
# -----------------------------------------------------------------------

sign-arm64: app-arm64
	@test -n "$(CERT)" || (echo "Error: APPLE_DEVELOPER_CERTIFICATE_NAME is not set" && exit 1)
	xattr -cr "$(APP_ARM64)"
	codesign --deep --force --options runtime \
		--entitlements entitlements.plist \
		--sign "$(CERT)" \
		"$(APP_ARM64)"
	@echo "Signed: $(APP_ARM64)"

sign-x86_64: app-x86_64
	@test -n "$(CERT)" || (echo "Error: APPLE_DEVELOPER_CERTIFICATE_NAME is not set" && exit 1)
	xattr -cr "$(APP_X86)"
	codesign --deep --force --options runtime \
		--entitlements entitlements.plist \
		--sign "$(CERT)" \
		"$(APP_X86)"
	@echo "Signed: $(APP_X86)"

# -----------------------------------------------------------------------
# Notarization (per architecture; staples the DMG)
# -----------------------------------------------------------------------

notarize-arm64: sign-arm64
	@test -n "$(TEAM_ID)"   || (echo "Error: APPLE_DEVELOPER_TEAM_ID is not set"      && exit 1)
	@test -n "$(APPLE_ID_)" || (echo "Error: APPLE_ID is not set"                     && exit 1)
	@test -n "$(APP_PASS)"  || (echo "Error: APPLE_DEVELOPER_APP_PASSWORD is not set" && exit 1)
	@command -v dmgbuild >/dev/null 2>&1 || (echo "Error: dmgbuild not found. Run: pipx install dmgbuild" && exit 1)
	dmgbuild -s "$(DMG_SETTINGS)" -D app="$(APP_ARM64)" "$(APP_NAME)" "$(APP_DMG_ARM64)"
	xcrun notarytool submit "$(APP_DMG_ARM64)" \
		--apple-id  "$(APPLE_ID_)" \
		--password  "$(APP_PASS)" \
		--team-id   "$(TEAM_ID)" \
		--wait
	xcrun stapler staple "$(APP_DMG_ARM64)"
	@echo "Notarized and stapled: $(APP_DMG_ARM64)"

notarize-x86_64: sign-x86_64
	@test -n "$(TEAM_ID)"   || (echo "Error: APPLE_DEVELOPER_TEAM_ID is not set"      && exit 1)
	@test -n "$(APPLE_ID_)" || (echo "Error: APPLE_ID is not set"                     && exit 1)
	@test -n "$(APP_PASS)"  || (echo "Error: APPLE_DEVELOPER_APP_PASSWORD is not set" && exit 1)
	@command -v dmgbuild >/dev/null 2>&1 || (echo "Error: dmgbuild not found. Run: pipx install dmgbuild" && exit 1)
	dmgbuild -s "$(DMG_SETTINGS)" -D app="$(APP_X86)" "$(APP_NAME)" "$(APP_DMG_X86)"
	xcrun notarytool submit "$(APP_DMG_X86)" \
		--apple-id  "$(APPLE_ID_)" \
		--password  "$(APP_PASS)" \
		--team-id   "$(TEAM_ID)" \
		--wait
	xcrun stapler staple "$(APP_DMG_X86)"
	@echo "Notarized and stapled: $(APP_DMG_X86)"

# Both notarized DMGs. Intel support planned until macOS 30 GA.
mac-release: notarize-arm64 notarize-x86_64

# -----------------------------------------------------------------------
# Info.plist  (PLIST_CONTENTS overrides default for per-arch bundles)
# -----------------------------------------------------------------------

PLIST_CONTENTS ?=

.PHONY: _plist
_plist:
	@test -n "$(PLIST_CONTENTS)"
	@printf '%s\n' \
		'<?xml version="1.0" encoding="UTF-8"?>' \
		'<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
		'<plist version="1.0">' \
		'<dict>' \
		'	<key>CFBundleName</key><string>$(APP_NAME)</string>' \
		'	<key>CFBundleIdentifier</key><string>$(BUNDLE_ID)</string>' \
		'	<key>CFBundleExecutable</key><string>$(EXE_NAME)</string>' \
		'	<key>CFBundleVersion</key><string>$(VERSION)</string>' \
		'	<key>CFBundleShortVersionString</key><string>$(VERSION)</string>' \
		'	<key>CFBundlePackageType</key><string>APPL</string>' \
		'	<key>CFBundleDevelopmentRegion</key><string>en</string>' \
		'	<key>CFBundleLocalizations</key>' \
		'	<array><string>en</string><string>ja</string></array>' \
		'	<key>LSMinimumSystemVersion</key><string>$(MIN_MACOS)</string>' \
		'	<key>NSPrincipalClass</key><string>NSApplication</string>' \
		'	<key>NSHighResolutionCapable</key><true/>' \
		'	<key>LSUIElement</key><true/>' \
		'	<key>CFBundleIconFile</key><string>AppIcon</string>' \
		'	<key>NSHumanReadableCopyright</key><string>Copyright 2026 Rino, eMotionGraphics Inc.</string>' \
		'</dict>' \
		'</plist>' \
		> "$(PLIST_CONTENTS)/Info.plist"

# -----------------------------------------------------------------------
# App icon (ICNS_RES_DIR / ICNS_OUT override for per-arch bundles)
# -----------------------------------------------------------------------

ICNS_RES_DIR ?=
ICNS_OUT       ?=

.PHONY: _icns_if_present
_icns_if_present:
	@test -n "$(ICNS_RES_DIR)" && test -n "$(ICNS_OUT)"
	@if [ -f "$(ICON_SRC)" ]; then \
		$(MAKE) _icns_build ICNS_RES_DIR="$(ICNS_RES_DIR)" ICNS_OUT="$(ICNS_OUT)"; \
	else \
		echo "Note: $(ICON_SRC) not found — skipping icon generation."; \
	fi

.PHONY: _icns_build
_icns_build:
	@test -n "$(ICNS_RES_DIR)" && test -n "$(ICNS_OUT)"
	mkdir -p "$(ICONSET)"
	sips -z 16    16    $(ICON_SRC) --out "$(ICONSET)/icon_16x16.png"    >/dev/null
	sips -z 32    32    $(ICON_SRC) --out "$(ICONSET)/icon_16x16@2x.png" >/dev/null
	sips -z 32    32    $(ICON_SRC) --out "$(ICONSET)/icon_32x32.png"    >/dev/null
	sips -z 64    64    $(ICON_SRC) --out "$(ICONSET)/icon_32x32@2x.png" >/dev/null
	sips -z 128   128   $(ICON_SRC) --out "$(ICONSET)/icon_128x128.png"    >/dev/null
	sips -z 256   256   $(ICON_SRC) --out "$(ICONSET)/icon_128x128@2x.png" >/dev/null
	sips -z 256   256   $(ICON_SRC) --out "$(ICONSET)/icon_256x256.png"    >/dev/null
	sips -z 512   512   $(ICON_SRC) --out "$(ICONSET)/icon_256x256@2x.png" >/dev/null
	sips -z 512   512   $(ICON_SRC) --out "$(ICONSET)/icon_512x512.png"    >/dev/null
	sips -z 1024  1024  $(ICON_SRC) --out "$(ICONSET)/icon_512x512@2x.png" >/dev/null
	iconutil -c icns "$(ICONSET)" -o "$(ICNS_OUT)"
	rm -rf "$(ICONSET)"

# -----------------------------------------------------------------------

clean:
	rm -rf "$(DIST_DIR)"

help:
	@echo "Targets:"
	@echo "  dev              app-arm64 + win (unsigned, daily development)"
	@echo "  app              app-arm64 + app-x86_64 (unsigned, local test)"
	@echo "  all              mac-release + win-zip (full release)"
	@echo "  mac-release      notarized DMGs for arm64 and x86_64"
	@echo "  mac-zip-arm64    Sparkle-ready zip (not in all)"
	@echo "  mac-zip-x86_64   Sparkle-ready zip (not in all)"
