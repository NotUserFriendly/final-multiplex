# Final Multiplex — dev build helpers.
#
# `make` / `make dev`   — debug build + populate target/debug/adapters/
# `make release`        — release build + populate target/release/adapters/
#
# After `make dev`, launch from any cwd without FM_ADAPTER_DIR:
#   target/debug/final-multiplex my-scene.toml
# or:
#   cargo run -- my-scene.toml   (cargo run rebuilds only fm-app; adapters stay)

ADAPTERS := fm-rtsp-adapter fm-dummy-adapter

.PHONY: dev release

dev:
	cargo build --workspace
	mkdir -p target/debug/adapters
	$(foreach a,$(ADAPTERS),cp target/debug/$(a) target/debug/adapters/$(a);)

release:
	cargo build --workspace --release
	mkdir -p target/release/adapters
	$(foreach a,$(ADAPTERS),cp target/release/$(a) target/release/adapters/$(a);)
