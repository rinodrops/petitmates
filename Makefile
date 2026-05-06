empty     :=
space     := $(empty) $(empty)

APP_NAME  := Petit Mates
EXE_NAME  := petitmates
BUNDLE_ID := jp.emotiongraphics.petitmates
VERSION   := 0.1.0
MIN_MACOS := 13.0

BUILD_DIR := build
APP       := $(BUILD_DIR)/$(APP_NAME).app
CONTENTS  := $(APP)/Contents
MACOS_DIR := $(CONTENTS)/MacOS
RES_DIR   := $(CONTENTS)/Resources
EXE       := $(MACOS_DIR)/$(EXE_NAME)

APP_ZIP   := $(BUILD_DIR)/Petit-Mates-v$(VERSION)-darwin-universal.zip
WIN_DIR     := $(BUILD_DIR)/petitmates-windows
WIN_EXE_NAME := Petit Mates
WIN_EXE   := $(WIN_DIR)/$(WIN_EXE_NAME).exe
WIN_ZIP   := $(BUILD_DIR)/Petit-Mates-v$(VERSION)-windows-x86_64.zip
WIN_TARGET_DIR := /tmp/pm-win

# Make-target–safe versions: spaces escaped as '\ ' for use in
# prerequisite lists and target definitions.
APP_T       := $(subst $(space),\ ,$(APP))
CONTENTS_T  := $(subst $(space),\ ,$(CONTENTS))
MACOS_DIR_T := $(subst $(space),\ ,$(MACOS_DIR))
RES_DIR_T   := $(subst $(space),\ ,$(RES_DIR))

CHAR_SRC  := assets/bearded_dragon
ICON_SRC  := assets/appicon.png
ICONSET   := $(BUILD_DIR)/AppIcon.iconset
ICNS      := $(RES_DIR)/AppIcon.icns

CERT      := $(APPLE_DEVELOPER_CERTIFICATE_NAME)
TEAM_ID   := $(APPLE_DEVELOPER_TEAM_ID)
APPLE_ID_ := $(APPLE_ID)
APP_PASS  := $(APPLE_DEVELOPER_APP_PASSWORD)

.PHONY: all app dev win win-zip mac-zip sign notarize inspect-mac inspect-win clean

all: app

# -----------------------------------------------------------------------
# Development build (current arch only, fast)
# -----------------------------------------------------------------------

dev: | $(MACOS_DIR_T) $(RES_DIR_T)
	cargo build --release
	cp target/release/$(EXE_NAME) "$(EXE)"
	mkdir -p "$(RES_DIR)/assets/bearded_dragon/sprite"
	cp $(CHAR_SRC)/manifest.toml   "$(RES_DIR)/assets/bearded_dragon/"
	cp $(CHAR_SRC)/config.toml     "$(RES_DIR)/assets/bearded_dragon/"
	cp $(CHAR_SRC)/sprite/*.png    "$(RES_DIR)/assets/bearded_dragon/sprite/"
	$(MAKE) _plist _icns_if_present
	@echo "Dev build: $(APP)"

# -----------------------------------------------------------------------
# Universal release build
# -----------------------------------------------------------------------

app: | $(MACOS_DIR_T) $(RES_DIR_T)
	MACOSX_DEPLOYMENT_TARGET=$(MIN_MACOS) cargo build --release --target aarch64-apple-darwin
	MACOSX_DEPLOYMENT_TARGET=$(MIN_MACOS) cargo build --release --target x86_64-apple-darwin
	lipo -create -output "$(EXE)" \
		target/aarch64-apple-darwin/release/$(EXE_NAME) \
		target/x86_64-apple-darwin/release/$(EXE_NAME)
	mkdir -p "$(RES_DIR)/assets/bearded_dragon/sprite"
	cp $(CHAR_SRC)/manifest.toml   "$(RES_DIR)/assets/bearded_dragon/"
	cp $(CHAR_SRC)/config.toml     "$(RES_DIR)/assets/bearded_dragon/"
	cp $(CHAR_SRC)/sprite/*.png    "$(RES_DIR)/assets/bearded_dragon/sprite/"
	$(MAKE) _plist _icns_if_present
	@echo "App bundle: $(APP)"

# -----------------------------------------------------------------------
# Info.plist  (always re-generated so version changes propagate)
# -----------------------------------------------------------------------

.PHONY: _plist
_plist: | $(CONTENTS_T)
	@rm -f "$(CONTENTS)/Info.plist"
	/usr/libexec/PlistBuddy \
		-c "Add :CFBundleName               string $(APP_NAME)" \
		-c "Add :CFBundleIdentifier         string $(BUNDLE_ID)" \
		-c "Add :CFBundleExecutable         string $(EXE_NAME)" \
		-c "Add :CFBundleVersion            string $(VERSION)" \
		-c "Add :CFBundleShortVersionString  string $(VERSION)" \
		-c "Add :CFBundlePackageType        string APPL" \
		-c "Add :LSMinimumSystemVersion     string $(MIN_MACOS)" \
		-c "Add :NSPrincipalClass           string NSApplication" \
		-c "Add :NSHighResolutionCapable    bool   YES" \
		-c "Add :LSUIElement                bool   YES" \
		-c "Add :CFBundleIconFile           string AppIcon" \
		-c "Add :NSHumanReadableCopyright   string Copyright 2026 eMotionGraphics Inc." \
		"$(CONTENTS)/Info.plist"

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

win:
	CARGO_TARGET_DIR="$(WIN_TARGET_DIR)" cargo build --release --target x86_64-pc-windows-gnu
	mkdir -p "$(WIN_DIR)"
	cp "$(WIN_TARGET_DIR)/x86_64-pc-windows-gnu/release/$(EXE_NAME).exe" "$(WIN_EXE)"
	@echo "Windows build: $(WIN_DIR)"

win-zip: win
	cd "$(BUILD_DIR)" && zip "$(notdir $(WIN_ZIP))" "$(notdir $(WIN_EXE))"
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
# Distribution zip (macOS)
# -----------------------------------------------------------------------

mac-zip: app
	ditto -c -k --keepParent "$(APP)" "$(APP_ZIP)"
	@echo "Package: $(APP_ZIP)"

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
	ditto -c -k --keepParent "$(APP)" "$(APP_ZIP)"
	xcrun notarytool submit "$(APP_ZIP)" \
		--apple-id  "$(APPLE_ID_)" \
		--password  "$(APP_PASS)" \
		--team-id   "$(TEAM_ID)" \
		--wait
	xcrun stapler staple "$(APP)"
	@echo "Notarized and stapled: $(APP)"

# -----------------------------------------------------------------------
# Directory scaffolding
# -----------------------------------------------------------------------

$(BUILD_DIR):
	mkdir -p "$(BUILD_DIR)"

$(CONTENTS_T): | $(BUILD_DIR)
	mkdir -p "$(CONTENTS)"

$(MACOS_DIR_T): | $(CONTENTS_T)
	mkdir -p "$(MACOS_DIR)"

$(RES_DIR_T): | $(CONTENTS_T)
	mkdir -p "$(RES_DIR)"

# -----------------------------------------------------------------------

clean:
	rm -rf "$(BUILD_DIR)"
