empty     :=
space     := $(empty) $(empty)

APP_NAME  := Petit Mates
EXE_NAME  := petitmates
BUNDLE_ID := jp.emotiongraphics.petitmates
VERSION   := $(shell awk -F'"' '/^version *=/{print $$2; exit}' Cargo.toml)
MIN_MACOS := 13.0

DIST_DIR  := dist
APP       := $(DIST_DIR)/$(APP_NAME).app
CONTENTS  := $(APP)/Contents
MACOS_DIR := $(CONTENTS)/MacOS
RES_DIR   := $(CONTENTS)/Resources
EXE       := $(MACOS_DIR)/$(EXE_NAME)

APP_ZIP   := $(DIST_DIR)/Petit-Mates-v$(VERSION)-darwin-universal.zip
APP_DMG   := $(DIST_DIR)/Petit-Mates-v$(VERSION)-darwin-universal.dmg
DMG_SETTINGS := dmg_settings.py
WIN_DIR     := $(DIST_DIR)/petitmates-windows
WIN_EXE_NAME := Petit Mates
WIN_EXE   := $(WIN_DIR)/$(WIN_EXE_NAME).exe
WIN_SETTINGS_EXE := Settings.exe
WIN_ZIP   := $(DIST_DIR)/Petit-Mates-v$(VERSION)-windows-x86_64.zip
WIN_TARGET_DIR := /tmp/pm-win

# Make-target–safe versions: spaces escaped as '\ ' for use in
# prerequisite lists and target definitions.
APP_T       := $(subst $(space),\ ,$(APP))
CONTENTS_T  := $(subst $(space),\ ,$(CONTENTS))
MACOS_DIR_T := $(subst $(space),\ ,$(MACOS_DIR))
RES_DIR_T   := $(subst $(space),\ ,$(RES_DIR))

BD_SRC    := assets/bearded_dragon
PT_SRC    := assets/pond_turtle
LG_SRC    := assets/leopard_gecko
ICON_SRC  := assets/appicon.png
ICONSET   := $(DIST_DIR)/AppIcon.iconset
ICNS      := $(RES_DIR)/AppIcon.icns

CERT      := $(APPLE_DEVELOPER_CERTIFICATE_NAME)
TEAM_ID   := $(APPLE_DEVELOPER_TEAM_ID)
APPLE_ID_ := $(APPLE_ID)
APP_PASS  := $(APPLE_DEVELOPER_APP_PASSWORD)

# Settings UI (local dev: repos/settings as ../settings; CI: checkout at ./settings)
SETTINGS_DIR     ?= ../settings
SETTINGS_SCHEMA  := $(abspath schema.toml)
SETTINGS_BIN     := $(SETTINGS_DIR)/target/release/settings
SETTINGS_WIN_EXE := $(SETTINGS_DIR)/dist/settings-windows/Settings.exe

.PHONY: all app dev win win-zip mac-zip mac-dmg sign notarize inspect-mac inspect-win clean settings settings-win

# Invoke cargo directly so SETTINGS_SCHEMA paths may contain spaces.
settings:
	@test -f '$(SETTINGS_DIR)/assets/icons.ttf' || $(MAKE) -C '$(SETTINGS_DIR)' icons
	cd '$(SETTINGS_DIR)' && SETTINGS_SCHEMA='$(SETTINGS_SCHEMA)' cargo build --release

settings-win:
	@test -f '$(SETTINGS_DIR)/assets/icons.ttf' || $(MAKE) -C '$(SETTINGS_DIR)' icons
	@test -f '$(SETTINGS_DIR)/assets/appicon.ico' || $(MAKE) -C '$(SETTINGS_DIR)' appicon-ico
	cd '$(SETTINGS_DIR)' && SETTINGS_SCHEMA='$(SETTINGS_SCHEMA)' CARGO_TARGET_DIR=/tmp/settings-win \
		cargo build --release --target x86_64-pc-windows-gnu
	@mkdir -p '$(SETTINGS_DIR)/dist/settings-windows'
	cp /tmp/settings-win/x86_64-pc-windows-gnu/release/settings.exe '$(SETTINGS_WIN_EXE)'

all: app

# -----------------------------------------------------------------------
# Development build (current arch only, fast)
# -----------------------------------------------------------------------

dev: settings | $(MACOS_DIR_T) $(RES_DIR_T)
	cargo build --release
	cp target/release/$(EXE_NAME) "$(EXE)"
	cp "$(SETTINGS_BIN)" "$(MACOS_DIR)/settings"
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
	$(MAKE) _plist _icns_if_present
	@echo "Dev build: $(APP)"

# -----------------------------------------------------------------------
# Universal release build
# -----------------------------------------------------------------------

app: settings | $(MACOS_DIR_T) $(RES_DIR_T)
	MACOSX_DEPLOYMENT_TARGET=$(MIN_MACOS) cargo build --release --target aarch64-apple-darwin
	MACOSX_DEPLOYMENT_TARGET=$(MIN_MACOS) cargo build --release --target x86_64-apple-darwin
	lipo -create -output "$(EXE)" \
		target/aarch64-apple-darwin/release/$(EXE_NAME) \
		target/x86_64-apple-darwin/release/$(EXE_NAME)
	cp "$(SETTINGS_BIN)" "$(MACOS_DIR)/settings"
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
	$(MAKE) _plist _icns_if_present
	@echo "App bundle: $(APP)"

# -----------------------------------------------------------------------
# Info.plist  (always re-generated so version changes propagate)
# -----------------------------------------------------------------------

.PHONY: _plist
_plist: | $(CONTENTS_T)
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
		> "$(CONTENTS)/Info.plist"

# -----------------------------------------------------------------------
# App icon (skip gracefully when assets/appicon.png does not exist)
# -----------------------------------------------------------------------

.PHONY: _icns_if_present
_icns_if_present: | $(RES_DIR_T)
	@if [ -f "$(ICON_SRC)" ]; then \
		$(MAKE) _icns_build; \
	else \
		echo "Note: $(ICON_SRC) not found — skipping icon generation."; \
	fi

.PHONY: _icns_build
_icns_build: | $(RES_DIR_T)
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
	iconutil -c icns "$(ICONSET)" -o "$(ICNS)"
	rm -rf "$(ICONSET)"

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
# Diagnostic tools (developer only, not included in distribution)
# -----------------------------------------------------------------------

inspect-mac:
	cargo build --bin wm_inspect
	@echo "Built: target/debug/wm_inspect"
	@echo "Run:   ./target/debug/wm_inspect"

inspect-win:
	CARGO_TARGET_DIR="$(WIN_TARGET_DIR)" cargo build --bin wm_inspect_win \
		--features inspect-win --target x86_64-pc-windows-gnu
	@echo "Built: $(WIN_TARGET_DIR)/x86_64-pc-windows-gnu/debug/wm_inspect_win.exe"

# -----------------------------------------------------------------------
# Distribution (macOS)
# -----------------------------------------------------------------------

mac-zip: app
	ditto -c -k --keepParent "$(APP)" "$(APP_ZIP)"
	@echo "Package: $(APP_ZIP)"

mac-dmg: app
	@command -v dmgbuild >/dev/null 2>&1 || (echo "Error: dmgbuild not found. Run: pipx install dmgbuild" && exit 1)
	dmgbuild -s "$(DMG_SETTINGS)" -D app="$(APP)" "$(APP_NAME)" "$(APP_DMG)"
	@echo "Package: $(APP_DMG)"

# -----------------------------------------------------------------------
# Code signing
# -----------------------------------------------------------------------

sign: app
	@test -n "$(CERT)" || (echo "Error: APPLE_DEVELOPER_CERTIFICATE_NAME is not set" && exit 1)
	xattr -cr "$(APP)"
	codesign --deep --force --options runtime \
		--entitlements entitlements.plist \
		--sign "$(CERT)" \
		"$(APP)"
	@echo "Signed: $(APP)"

# -----------------------------------------------------------------------
# Notarization
# -----------------------------------------------------------------------

notarize: sign
	@test -n "$(TEAM_ID)"   || (echo "Error: APPLE_DEVELOPER_TEAM_ID is not set"      && exit 1)
	@test -n "$(APPLE_ID_)" || (echo "Error: APPLE_ID is not set"                     && exit 1)
	@test -n "$(APP_PASS)"  || (echo "Error: APPLE_DEVELOPER_APP_PASSWORD is not set" && exit 1)
	@command -v dmgbuild >/dev/null 2>&1 || (echo "Error: dmgbuild not found. Run: pipx install dmgbuild" && exit 1)
	dmgbuild -s "$(DMG_SETTINGS)" -D app="$(APP)" "$(APP_NAME)" "$(APP_DMG)"
	xcrun notarytool submit "$(APP_DMG)" \
		--apple-id  "$(APPLE_ID_)" \
		--password  "$(APP_PASS)" \
		--team-id   "$(TEAM_ID)" \
		--wait
	xcrun stapler staple "$(APP_DMG)"
	@echo "Notarized and stapled: $(APP_DMG)"

# -----------------------------------------------------------------------
# Directory scaffolding
# -----------------------------------------------------------------------

$(DIST_DIR):
	mkdir -p "$(DIST_DIR)"

$(CONTENTS_T): | $(DIST_DIR)
	mkdir -p "$(CONTENTS)"

$(MACOS_DIR_T): | $(CONTENTS_T)
	mkdir -p "$(MACOS_DIR)"

$(RES_DIR_T): | $(CONTENTS_T)
	mkdir -p "$(RES_DIR)"

# -----------------------------------------------------------------------

clean:
	rm -rf "$(DIST_DIR)"
