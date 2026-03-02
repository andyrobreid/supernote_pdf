set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

# Build release binary locally
build:
	cargo build --release

# Install supernote_pdf globally from this local checkout
# Equivalent to: cargo install --path . --force
# (force is used so local changes are reinstalled)
deploy_local: build
	cargo install --path . --force
	echo "Installed supernote_pdf globally from $(pwd)"
