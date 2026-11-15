# Exergame build commands. All Rust builds run inside the exergame container:
# the toolchain (ONNX Runtime, CUDA, cuDNN, the Android SDK) lives in the
# image, not on the host. Artifacts land in src-tauri/Exergame/src-tauri/target
# and gen/android (host-visible through the /srv mount).

APP_DIR := src-tauri/Exergame/src-tauri

# Deferred (`=`) on purpose: the wildcard must resolve AFTER the build runs,
# when the freshly built artifact exists.
APPIMAGE = $(wildcard $(APP_DIR)/target/release/bundle/appimage/*.AppImage)
APK_RELEASE := $(APP_DIR)/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
APK_DEBUG := $(APP_DIR)/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
AAB_RELEASE := $(APP_DIR)/gen/android/app/build/outputs/bundle/universalRelease/app-universal-release.aab

# App version from tauri.conf.json, used for the GitHub release tag.
VERSION = $(shell grep -oP '(?<=\"version\": \")[^\"]+' $(APP_DIR)/tauri.conf.json | head -1)

# Resolved at recipe time (the file may carry spaces in its name, which make
# prerequisites cannot handle — everything below quotes it inside recipes).
APPIMAGE_FILE = $(shell find $(APP_DIR)/target/release/bundle/appimage -maxdepth 1 -name '*.AppImage' | head -1)

DIST = dist
DIST_APPIMAGE = $(DIST)/Movenet-Demo-$(VERSION)-linux-x86_64.AppImage
DIST_APK = $(DIST)/Movenet-Demo-$(VERSION)-android-universal.apk
DIST_AAB = $(DIST)/Movenet-Demo-$(VERSION)-android.aab

# All builds share target/ (including the runtime bundle directory), so they
# are serialized with flock: parallel invocations wait instead of racing.
LOCK := /tmp/exergame-build.lock

.PHONY: help appimage android android-debug dev clean clean-all release upload

help:
	@echo "make appimage      - desktop AppImage (GPU ONNX Runtime bundle)"
	@echo "make android       - signed release APK + AAB"
	@echo "make android-debug - debug APK for quick device testing"
	@echo "make dev           - dev environment (frontend + cargo tauri dev)"
	@echo "make clean         - remove debug build caches (release kept)"
	@echo "make clean-all     - remove the whole target directory"
	@echo "make release       - publish AppImage + APK + AAB as a GitHub release (gh)"
	@echo "make upload        - re-upload artifacts to the existing release"

# Desktop: release build packaged as an AppImage with the GPU runtime bundle.
appimage:
	flock $(LOCK) docker compose run --rm exergame sh -c "cd /srv/src-tauri && cargo tauri build --bundles appimage"
	@echo "Artifact: $(APPIMAGE)"

# Android: release build, APK and AAB signed with the exergame keystore.
android:
	flock $(LOCK) docker compose run --rm exergame sh -c "cd /srv/src-tauri && cargo tauri android build"
	@echo "APK: $(APK_RELEASE)"
	@echo "AAB: $(AAB_RELEASE)"

# Android: debug APK for quick on-device testing.
android-debug:
	flock $(LOCK) docker compose run --rm exergame sh -c "cd /srv/src-tauri && cargo tauri android build --debug"
	@echo "APK: $(APK_DEBUG)"

# Dev environment: starts the vite dev server and cargo tauri dev.
dev:
	docker compose up frontend exergame

# Remove the debug build caches. Runs in the container because the build
# files are created by root; release artifacts are kept.
clean:
	flock $(LOCK) docker compose run --rm exergame sh -c "rm -rf /srv/src-tauri/target/debug /srv/src-tauri/target/*/debug"

# Remove the whole target directory: the next build recompiles everything.
clean-all:
	flock $(LOCK) docker compose run --rm exergame sh -c "rm -rf /srv/src-tauri/target"

# Copy the release artifacts to dist/ with clean names and publish them as a
# GitHub release (tag v$(VERSION) is created at the current HEAD). Requires
# the gh CLI logged in (`gh auth login`). Run `make appimage` / `make android`
# first so the artifacts exist.
release:
	@mkdir -p $(DIST)
	cp "$(APPIMAGE_FILE)" "$(DIST_APPIMAGE)"
	cp "$(APK_RELEASE)" "$(DIST_APK)"
	cp "$(AAB_RELEASE)" "$(DIST_AAB)"
	gh release create v$(VERSION) "$(DIST_APPIMAGE)" "$(DIST_APK)" "$(DIST_AAB)" \
		--title "Movenet Demo $(VERSION)" --generate-notes
	@echo "Release v$(VERSION) published."

# Re-upload the artifacts to the existing release, replacing old files.
upload:
	@mkdir -p $(DIST)
	cp "$(APPIMAGE_FILE)" "$(DIST_APPIMAGE)"
	cp "$(APK_RELEASE)" "$(DIST_APK)"
	cp "$(AAB_RELEASE)" "$(DIST_AAB)"
	gh release upload v$(VERSION) "$(DIST_APPIMAGE)" "$(DIST_APK)" "$(DIST_AAB)" --clobber
	@echo "Artifacts uploaded to release v$(VERSION)."
