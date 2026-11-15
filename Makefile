# Exergame build commands. All Rust builds run inside the exergame container:
# the toolchain (ONNX Runtime, CUDA, cuDNN, the Android SDK) lives in the
# image, not on the host. Artifacts land in src-tauri/Exergame/src-tauri/target
# and gen/android (host-visible through the /srv mount).

APP_DIR := src-tauri/Exergame/src-tauri

APPIMAGE := $(wildcard $(APP_DIR)/target/release/bundle/appimage/*.AppImage)
APK_RELEASE := $(APP_DIR)/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
APK_DEBUG := $(APP_DIR)/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
AAB_RELEASE := $(APP_DIR)/gen/android/app/build/outputs/bundle/universalRelease/app-universal-release.aab

.PHONY: help appimage android android-debug dev clean clean-all

help:
	@echo "make appimage      - desktop AppImage (GPU ONNX Runtime bundle)"
	@echo "make android       - signed release APK + AAB"
	@echo "make android-debug - debug APK for quick device testing"
	@echo "make dev           - dev environment (frontend + cargo tauri dev)"
	@echo "make clean         - remove debug build caches (release kept)"
	@echo "make clean-all     - remove the whole target directory"

# Desktop: release build packaged as an AppImage with the GPU runtime bundle.
appimage:
	docker compose run --rm exergame sh -c "cd /srv/src-tauri && cargo tauri build --bundles appimage"
	@echo "Artifact: $(APPIMAGE)"

# Android: release build, APK and AAB signed with the exergame keystore.
android:
	docker compose run --rm exergame sh -c "cd /srv/src-tauri && cargo tauri android build"
	@echo "APK: $(APK_RELEASE)"
	@echo "AAB: $(AAB_RELEASE)"

# Android: debug APK for quick on-device testing.
android-debug:
	docker compose run --rm exergame sh -c "cd /srv/src-tauri && cargo tauri android build --debug"
	@echo "APK: $(APK_DEBUG)"

# Dev environment: starts the vite dev server and cargo tauri dev.
dev:
	docker compose up frontend exergame

# Remove the debug build caches. Runs in the container because the build
# files are created by root; release artifacts are kept.
clean:
	docker compose run --rm exergame sh -c "rm -rf /srv/src-tauri/target/debug /srv/src-tauri/target/*/debug"

# Remove the whole target directory: the next build recompiles everything.
clean-all:
	docker compose run --rm exergame sh -c "rm -rf /srv/src-tauri/target"
