.PHONY: build run dev debug test check dmg
build:
	./scripts/build-macos
run: build
	open dist/Seams.app
dev:
	./scripts/dev-macos
debug:
	cargo build --locked --bin seams
	rust-lldb target/debug/seams
test:
	cargo test --locked
check:
	cargo fmt --check
	cargo clippy --locked --all-targets -- -D warnings
dmg: build
	hdiutil create -volname AppSeams -srcfolder dist/Seams.app -ov -format UDZO dist/AppSeams.dmg
