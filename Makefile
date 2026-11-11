NPM ?= npm
TAURI := $(NPM) run tauri --

TARGET_DIR ?= target/release
BUNDLE_DIR := $(TARGET_DIR)/bundle

.DEFAULT_GOAL := help

.PHONY: help dev app dmg test typecheck clean distclean

help: ## Show these targets
	@echo "Targets:"
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-11s %s\n", $$1, $$2}'

node_modules: package.json package-lock.json
	$(NPM) ci

dev: node_modules ## Run the app with hot reload
	$(TAURI) dev

app: node_modules ## Build the .app bundle, stopping before the dmg step
	$(TAURI) build --bundles app

# The dmg step lays out the disk image by driving Finder through AppleScript, so it
# needs Automation permission; `-1743` means the shell running it does not have one,
# and every failed attempt leaves a ~30 MB partial `rw.*.dmg` next to the .app.
DMG_TEMP := "$(BUNDLE_DIR)/macos"/rw.*.dmg

dmg: node_modules ## Build the .app and the .dmg installer
	@rm -f $(DMG_TEMP)
	@$(TAURI) build --bundles app,dmg || { rm -f $(DMG_TEMP); \
		echo "The .app is built but the dmg step failed: it needs Automation permission."; \
		echo "Grant it to your terminal in System Settings, or run 'make app' to stop before it."; \
		exit 1; }
	@echo "Built:"
	@ls -1 "$(BUNDLE_DIR)/dmg"/*.dmg

test: ## Run the engine and IPC test suites
	cargo test --workspace

typecheck: node_modules ## Typecheck the frontend
	$(NPM) run typecheck

clean: ## Remove the frontend and bundle outputs
	rm -rf dist "$(BUNDLE_DIR)"

distclean: clean ## Also remove the cargo target directory
	rm -rf target
