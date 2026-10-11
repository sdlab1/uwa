.PHONY: help build ui verify demo clean

help:            ## Show this help
	@awk 'BEGIN {FS = ":.*##"; printf "Usage: make <target>\n\n"} \
	     /^[a-zA-Z_-]+:.*?##/ {printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build:           ## cargo build --release + UI bundle
	cargo build --release
	cd crates/uwa-api/static && npm ci --silent && npm run --silent build

ui:              ## Rebuild only the dashboard
	cd crates/uwa-api/static && npm run --silent build

verify:          ## Full local check (Rust + typecheck + E2E)
	./verify.sh

demo:            ## Regenerate docs/demo.gif (requires vhs)
	@command -v vhs >/dev/null || { echo "install vhs: https://github.com/charmbracelet/vhs#installation"; exit 1; }
	vhs docs/demo.tape
	@ls -lh docs/demo.gif

clean:           ## Remove build artifacts
	cargo clean
	rm -rf crates/uwa-api/static/dist crates/uwa-api/static/node_modules