.PHONY: build run dev debug test check dmg
build:
	./scripts/build-macos
run: build
	open dist/omarchy-launchpad.app
dev:
	./scripts/dev-macos
debug:
	cargo build --locked --bin omarchy-launchpad
	rust-lldb target/debug/omarchy-launchpad
test:
	cargo test --locked
check:
	cargo fmt --check
	cargo clippy --locked --all-targets -- -D warnings
dmg: build
	hdiutil create -volname omarchy-launchpad -srcfolder dist/omarchy-launchpad.app -ov -format UDZO dist/omarchy-launchpad.dmg
