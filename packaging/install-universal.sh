#!/bin/bash
# Nellie Universal Installer
# Usage: curl -sSL https://github.com/mmorris35/nellie/releases/latest/download/install-universal.sh | bash
#
# Or with specific version:
# curl -sSL https://github.com/mmorris35/nellie/releases/download/v0.1.0/install-universal.sh | bash

set -euo pipefail

REPO="mmorris35/nellie"
INSTALL_DIR="${NELLIE_INSTALL_DIR:-$HOME/.local/share/nellie}"
BIN_DIR="${NELLIE_BIN_DIR:-$HOME/.local/bin}"

# ONNX Runtime version — must match src/embeddings/version.rs MIN_ORT_VERSION
ORT_VERSION="1.24.4"
ORT_SHA256_LINUX_X64="3a211fbea252c1e66290658f1b735b772056149f28321e71c308942cdb54b747"
ORT_SHA256_LINUX_ARM64="866109a9248d057671a039b9d725be4bd86888e3754140e6701ec621be9d4d7e"
ORT_SHA256_MACOS_ARM64="93787795f47e1eee369182e43ed51b9e5da0878ab0346aecf4258979b8bba989"

NELLIE_DATA_DIR="${NELLIE_DATA_DIR:-$HOME/.local/share/nellie}"
NELLIE_LIB_DIR="${NELLIE_DATA_DIR}/lib"

# Embedding model URLs and checksums (all-MiniLM-L6-v2)
MODEL_URL="https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx"
MODEL_SHA256="6fd5d72fe4589f189f8ebc006442dbb529bb7ce38f8082112682524616046452"
TOKENIZER_URL="https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json"
TOKENIZER_SHA256="be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037"
NELLIE_MODELS_DIR="${NELLIE_DATA_DIR}/models"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info() { echo -e "${GREEN}==>${NC} $1"; }
warn() { echo -e "${YELLOW}Warning:${NC} $1"; }
error() { echo -e "${RED}Error:${NC} $1" >&2; exit 1; }

# Portable SHA-256 checksum: macOS uses `shasum -a 256`, Linux uses `sha256sum`
portable_sha256() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | awk '{print $1}'
    else
        error "No SHA-256 tool found. Install sha256sum or shasum."
    fi
}

# Detect OS and architecture
detect_platform() {
    local os arch

    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)  os="linux" ;;
        Darwin) os="macos" ;;
        *)      error "Unsupported OS: $os" ;;
    esac

    case "$arch" in
        x86_64|amd64)  arch="x86_64" ;;
        arm64|aarch64) arch="aarch64" ;;
        *)             error "Unsupported architecture: $arch" ;;
    esac

    echo "nellie-${os}-${arch}"
}

# Prime sudo credentials for non-interactive install
prime_sudo() {
    # If build prereqs are already installed, skip this entirely.
    if command -v gcc >/dev/null 2>&1 && command -v pkg-config >/dev/null 2>&1; then
        info "No sudo required (build prereqs present)"
        return 0
    fi

    # Try non-interactive sudo first (passwordless / NOPASSWD / cached creds).
    # This covers OrbStack VMs, CI runners, Docker, and Claude Code sessions
    # where there's no TTY to prompt for a password.
    if sudo -n true 2>/dev/null; then
        info "sudo available (non-interactive)"
        return 0
    fi

    # Fall back to interactive sudo -v (prompts for password once)
    info "Nellie installer needs sudo once to install build prereqs."
    info "You will be asked for your password ONE time, then the install runs non-interactively."
    if ! sudo -v; then
        error "sudo credentials not available. Either run this script as a user with sudo, \
or install prereqs manually: build-essential pkg-config libssl-dev"
    fi

    # Keep sudo credentials refreshed for the duration of the script
    ( while true; do sudo -n true; sleep 60; kill -0 "$$" 2>/dev/null || exit; done ) 2>/dev/null &
    SUDO_KEEPALIVE_PID=$!
    trap 'kill "$SUDO_KEEPALIVE_PID" 2>/dev/null || true' EXIT
}

# Install build prerequisites (gcc, pkg-config, libssl-dev)
install_build_prereqs() {
    info "Checking build prerequisites (gcc, pkg-config, libssl-dev)..."

    if command -v gcc >/dev/null 2>&1 && command -v pkg-config >/dev/null 2>&1; then
        info "Build prerequisites already present, skipping"
        return 0
    fi

    local os
    os="$(uname -s)"

    case "$os" in
        Linux)
            if [ -f /etc/debian_version ]; then
                info "Detected Debian/Ubuntu — installing build-essential pkg-config libssl-dev"
                sudo -n apt-get update -qq
                sudo -n apt-get install -y build-essential pkg-config libssl-dev
            elif [ -f /etc/redhat-release ]; then
                info "Detected RHEL/Fedora — installing gcc gcc-c++ make pkgconf openssl-devel"
                sudo -n dnf install -y gcc gcc-c++ make pkgconf openssl-devel
            elif [ -f /etc/arch-release ]; then
                info "Detected Arch — installing base-devel pkgconf openssl"
                sudo -n pacman -S --noconfirm base-devel pkgconf openssl
            elif [ -f /etc/alpine-release ]; then
                info "Detected Alpine — installing build-base pkgconfig openssl-dev"
                sudo -n apk add build-base pkgconfig openssl-dev
            else
                error "Unsupported Linux distribution. Install gcc, pkg-config, and libssl-dev manually, then re-run."
            fi
            ;;
        Darwin)
            if ! xcode-select -p >/dev/null 2>&1; then
                info "Installing Xcode Command Line Tools (may open a GUI prompt)"
                xcode-select --install || true
            fi
            ;;
        *)
            error "Unsupported OS for build prereqs: $os"
            ;;
    esac
}

MIN_RUST_VERSION="1.75.0"

version_ge() {
    # Returns 0 if $1 >= $2
    [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" = "$2" ]
}

install_rust_toolchain() {
    info "Checking Rust toolchain (need >= ${MIN_RUST_VERSION})..."

    if ! command -v cargo >/dev/null 2>&1; then
        info "cargo not found — installing rustup non-interactively"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
        # shellcheck disable=SC1090
        . "$HOME/.cargo/env"
    fi

    local rust_version
    rust_version="$(rustc --version | awk '{print $2}')"

    if ! version_ge "$rust_version" "$MIN_RUST_VERSION"; then
        error "Rust $rust_version is too old. Need >= $MIN_RUST_VERSION. Run: rustup update stable"
    fi

    info "Rust $rust_version (>= $MIN_RUST_VERSION) OK"
}

download_onnx_runtime() {
    local os arch tgz url expected_sha
    os="$(uname -s)"
    arch="$(uname -m)"

    case "${os}-${arch}" in
        Linux-x86_64)   tgz="onnxruntime-linux-x64-${ORT_VERSION}.tgz";    expected_sha="$ORT_SHA256_LINUX_X64" ;;
        Linux-aarch64)  tgz="onnxruntime-linux-aarch64-${ORT_VERSION}.tgz"; expected_sha="$ORT_SHA256_LINUX_ARM64" ;;
        Darwin-arm64)   tgz="onnxruntime-osx-arm64-${ORT_VERSION}.tgz";    expected_sha="$ORT_SHA256_MACOS_ARM64" ;;
        *) error "Unsupported platform for ONNX Runtime: ${os}-${arch}" ;;
    esac

    url="https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/${tgz}"

    mkdir -p "$NELLIE_LIB_DIR"
    if [ -f "$NELLIE_LIB_DIR/libonnxruntime.so" ] || [ -f "$NELLIE_LIB_DIR/libonnxruntime.dylib" ]; then
        info "ONNX Runtime already installed at $NELLIE_LIB_DIR, skipping"
        return 0
    fi

    info "Downloading ONNX Runtime ${ORT_VERSION} from Microsoft GitHub"
    local tmp
    tmp="$(mktemp -d)"
    curl -fsSL -o "$tmp/$tgz" "$url" || error "Failed to download $url"

    local actual_sha
    actual_sha="$(portable_sha256 "$tmp/$tgz")"
    if [ "$actual_sha" != "$expected_sha" ]; then
        error "Checksum mismatch for $tgz: expected $expected_sha, got $actual_sha"
    fi

    tar -xzf "$tmp/$tgz" -C "$tmp"
    cp "$tmp"/onnxruntime-*/lib/libonnxruntime.so*    "$NELLIE_LIB_DIR/" 2>/dev/null || true
    cp "$tmp"/onnxruntime-*/lib/libonnxruntime.dylib* "$NELLIE_LIB_DIR/" 2>/dev/null || true
    rm -rf "$tmp"

    info "ONNX Runtime installed to $NELLIE_LIB_DIR"
}

# Get latest release version (may return empty for repos without releases)
get_latest_version() {
    curl -sSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null | \
        grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/'
}

# Download binary from GitHub releases
download_binary() {
    local artifact="$1"
    local version="$2"
    local url="https://github.com/$REPO/releases/download/$version/$artifact"

    info "Downloading $artifact ($version)..."
    curl -sSL -o "$INSTALL_DIR/nellie" "$url" || error "Failed to download from $url"
    chmod +x "$INSTALL_DIR/nellie"
}

# Find the repo root directory for build-from-source.
# Checks: directory of this script, then CWD.
find_repo_root() {
    # Try the directory containing this script first
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    # Walk up from script dir (script lives in packaging/)
    local candidate
    for candidate in "$script_dir/.." "$script_dir" "$(pwd)" "$(pwd)/.."; do
        candidate="$(cd "$candidate" 2>/dev/null && pwd)" || continue
        if [ -f "$candidate/Cargo.toml" ] && grep -q "nellie" "$candidate/Cargo.toml" 2>/dev/null; then
            echo "$candidate"
            return 0
        fi
    done
    return 1
}

# Build from source and install the binary
build_from_source() {
    local repo_root="$1"
    info "Building nellie from source at $repo_root ..."
    (
        cd "$repo_root"
        export ORT_DYLIB_PATH="$NELLIE_LIB_DIR/libonnxruntime.so"
        # Try .dylib if .so doesn't exist (macOS)
        if [ ! -f "$ORT_DYLIB_PATH" ] && [ -f "$NELLIE_LIB_DIR/libonnxruntime.dylib" ]; then
            export ORT_DYLIB_PATH="$NELLIE_LIB_DIR/libonnxruntime.dylib"
        fi
        cargo build --release 2>&1
    ) || return 1

    local binary="$repo_root/target/release/nellie"
    if [ ! -f "$binary" ]; then
        warn "Build succeeded but binary not found at $binary"
        return 1
    fi

    mkdir -p "$INSTALL_DIR"
    cp "$binary" "$INSTALL_DIR/nellie"
    chmod +x "$INSTALL_DIR/nellie"
    info "Installed nellie from source to $INSTALL_DIR/nellie"
    return 0
}

# Install the nellie binary: try build-from-source first, then GitHub release
install_nellie_binary() {
    mkdir -p "$INSTALL_DIR/logs"

    # Path 1: Build from source if we're inside a git clone
    local repo_root
    if repo_root="$(find_repo_root)"; then
        info "Detected nellie repo at $repo_root — building from source"
        if build_from_source "$repo_root"; then
            return 0
        fi
        warn "Build from source failed. Trying binary download..."
    fi

    # Path 2: Download pre-built binary from GitHub releases
    local artifact version
    artifact="$(detect_platform)"
    version="$(get_latest_version)"

    if [ -n "$version" ]; then
        info "Platform: $artifact"
        info "Version: $version"
        if download_binary "$artifact" "$version"; then
            return 0
        fi
        warn "Binary download failed."
    else
        warn "No GitHub release found (private repo or no releases yet)."
    fi

    # Path 3: Neither worked — tell the user what to do
    echo ""
    error "Could not install nellie binary.

To build manually:
  git clone https://github.com/$REPO.git && cd nellie
  cargo build --release
  cp target/release/nellie $INSTALL_DIR/nellie

Then re-run this script to set up the service."
}

download_embedding_model() {
    mkdir -p "$NELLIE_MODELS_DIR"
    local model_path="$NELLIE_MODELS_DIR/all-MiniLM-L6-v2.onnx"
    local tokenizer_path="$NELLIE_MODELS_DIR/tokenizer.json"

    if [ -f "$model_path" ] && [ -f "$tokenizer_path" ]; then
        info "Embedding model already present at $NELLIE_MODELS_DIR, skipping"
        return 0
    fi

    info "Downloading embedding model (all-MiniLM-L6-v2, ~87 MB)"
    curl -fsSL -o "$model_path" "$MODEL_URL" || error "Failed to download model from $MODEL_URL"
    local actual_sha
    actual_sha="$(portable_sha256 "$model_path")"
    if [ "$actual_sha" != "$MODEL_SHA256" ]; then
        error "Model checksum mismatch: expected $MODEL_SHA256, got $actual_sha"
    fi

    info "Downloading tokenizer"
    curl -fsSL -o "$tokenizer_path" "$TOKENIZER_URL" || error "Failed to download tokenizer from $TOKENIZER_URL"
    actual_sha="$(portable_sha256 "$tokenizer_path")"
    if [ "$actual_sha" != "$TOKENIZER_SHA256" ]; then
        error "Tokenizer checksum mismatch: expected $TOKENIZER_SHA256, got $actual_sha"
    fi

    info "Embedding model + tokenizer installed to $NELLIE_MODELS_DIR"
}

# Create default config
create_config() {
    local config_file="$INSTALL_DIR/config.toml"
    
    if [[ -f "$config_file" ]]; then
        info "Config already exists at $config_file"
        return
    fi
    
    info "Creating default configuration..."
    cat > "$config_file" << 'EOF'
# Nellie Configuration
# Edit this file to customize your Nellie instance

[server]
host = "127.0.0.1"
port = 8765

[storage]
# Database location (default: ~/.local/share/nellie/nellie.db)
# db_path = "/path/to/nellie.db"

[embeddings]
# Model path (default: ~/.local/share/nellie/models/all-MiniLM-L6-v2.onnx)
# model_path = "/path/to/model.onnx"

[watcher]
# Directories to watch for code changes
# Add your code directories here:
# watch_dirs = [
#     "/path/to/your/code",
#     "/path/to/another/project"
# ]

# File patterns to ignore (in addition to .gitignore)
# ignore_patterns = ["*.log", "node_modules", "target", ".git"]
EOF
    
    info "Config created at $config_file"
    warn "Edit $config_file to add your watch directories!"
}

# Setup shell integration
setup_shell() {
    local shell_rc=""
    local path_line="export PATH=\"$BIN_DIR:\$PATH\""
    
    # Detect shell
    case "$SHELL" in
        */zsh)  shell_rc="$HOME/.zshrc" ;;
        */bash) shell_rc="$HOME/.bashrc" ;;
        *)      shell_rc="" ;;
    esac
    
    # Create bin directory and symlink
    mkdir -p "$BIN_DIR"
    ln -sf "$INSTALL_DIR/nellie" "$BIN_DIR/nellie"
    
    # Add to PATH if needed
    if [[ -n "$shell_rc" ]] && ! grep -q "$BIN_DIR" "$shell_rc" 2>/dev/null; then
        echo "" >> "$shell_rc"
        echo "# Nellie" >> "$shell_rc"
        echo "$path_line" >> "$shell_rc"
        info "Added $BIN_DIR to PATH in $shell_rc"
        warn "Run 'source $shell_rc' or restart your terminal"
    fi
}

# Create launchd plist for macOS
setup_macos_service() {
    local plist_dir="$HOME/Library/LaunchAgents"
    local plist_file="$plist_dir/com.nellie.plist"
    
    mkdir -p "$plist_dir"
    
    cat > "$plist_file" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.nellie</string>
    <key>ProgramArguments</key>
    <array>
        <string>$INSTALL_DIR/nellie</string>
        <string>serve</string>
        <string>--host</string>
        <string>0.0.0.0</string>
        <string>--port</string>
        <string>8765</string>
        <string>--data-dir</string>
        <string>$HOME/.local/share/nellie</string>
        <string>--enable-graph</string>
        <string>--enable-structural</string>
        <string>--enable-deep-hooks</string>
        <string>--sync-interval</string>
        <string>30</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>ORT_DYLIB_PATH</key>
        <string>$HOME/.local/share/nellie/lib/libonnxruntime.so</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>$INSTALL_DIR/logs/nellie.log</string>
    <key>StandardErrorPath</key>
    <string>$INSTALL_DIR/logs/nellie.log</string>
    <key>WorkingDirectory</key>
    <string>$INSTALL_DIR</string>
</dict>
</plist>
EOF
    
    info "Created launchd service at $plist_file"
    
    # Auto-start the service
    launchctl unload "$plist_file" 2>/dev/null || true
    launchctl load "$plist_file"
    info "Started Nellie service"
}

# Create systemd service for Linux
setup_linux_service() {
    local service_dir="$HOME/.config/systemd/user"
    local service_file="$service_dir/nellie.service"
    
    mkdir -p "$service_dir"
    
    cat > "$service_file" << EOF
[Unit]
Description=Nellie Code Memory Server
After=network.target

[Service]
Type=simple
ExecStart=$INSTALL_DIR/nellie serve --host 0.0.0.0 --port 8765 --data-dir $HOME/.local/share/nellie --enable-graph --enable-structural --enable-deep-hooks --sync-interval 30
WorkingDirectory=$INSTALL_DIR
Environment=ORT_DYLIB_PATH=$HOME/.local/share/nellie/lib/libonnxruntime.so
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF
    
    info "Created systemd user service at $service_file"
    
    # Auto-start the service
    systemctl --user daemon-reload
    systemctl --user enable nellie
    systemctl --user restart nellie
    info "Started Nellie service"

    # enable-linger keeps user services running after logout (optional)
    if loginctl enable-linger "$USER" 2>/dev/null; then
        info "Enabled lingering for $USER (service survives logout)"
    else
        warn "Could not enable lingering. Service may stop when you log out."
        warn "Optional: run 'loginctl enable-linger $USER' for service to survive logout"
    fi
}

# Main installation
main() {
    echo ""
    echo "╔═══════════════════════════════════════════╗"
    echo "║     Nellie Installer                   ║"
    echo "║     Your AI-Powered Code Memory           ║"
    echo "╚═══════════════════════════════════════════╝"
    echo ""

    # Prime sudo credentials first
    prime_sudo

    # Install build prerequisites
    install_build_prereqs

    # Install Rust toolchain
    install_rust_toolchain

    # Download ONNX Runtime
    download_onnx_runtime

    # Download embedding model and tokenizer
    download_embedding_model

    echo ""

    # Install the nellie binary (build-from-source or download)
    install_nellie_binary
    create_config
    setup_shell

    # Bootstrap default lessons BEFORE starting the service.
    # The bootstrap command opens SQLite directly, so it must not run
    # while the service is also writing to the DB.
    echo ""
    info "Bootstrapping default lessons..."
    ORT_DYLIB_PATH="$NELLIE_LIB_DIR/libonnxruntime.so" "$BIN_DIR/nellie" bootstrap --data-dir "$NELLIE_DATA_DIR" 2>&1 || warn "Bootstrap failed (non-fatal)"

    # Setup service based on OS
    echo ""
    if [[ "$(uname -s)" == "Darwin" ]]; then
        setup_macos_service
    else
        setup_linux_service
    fi

    # Post-install verification
    info "Verifying installation..."

    # 1. Version check
    if ! "$BIN_DIR/nellie" --version >/dev/null 2>&1; then
        warn "nellie --version failed"
    fi

    # 2. Wait for server to be ready (up to 15 seconds)
    local ready=false
    for i in $(seq 1 15); do
        if curl -fsS "http://127.0.0.1:8765/health" >/dev/null 2>&1; then
            ready=true
            break
        fi
        sleep 1
    done

    if [ "$ready" = true ]; then
        info "Server health check passed"
    else
        warn "Server not responding on port 8765 after 15s"
    fi

    # 3. Verify bootstrap lessons
    local lesson_count
    lesson_count=$(curl -fsS -X POST http://127.0.0.1:8765/mcp/invoke \
        -H 'Content-Type: application/json' \
        -d '{"name":"list_lessons","arguments":{}}' 2>/dev/null | \
        grep -o '"lesson_' | wc -l | tr -d ' ')
    if [ "${lesson_count:-0}" -ge 8 ]; then
        info "Bootstrap lessons verified ($lesson_count lessons)"
    else
        warn "Expected >= 8 bootstrap lessons, found ${lesson_count:-0}"
    fi

    echo ""
    echo "═══════════════════════════════════════════"
    echo ""
    info "Installation complete!"
    echo ""
    echo "Quick start:"
    echo "  1. Edit config: $INSTALL_DIR/config.toml"
    echo "     Add your code directories to watch_dirs"
    echo ""
    echo "  2. Run manually:"
    echo "     $BIN_DIR/nellie --config $INSTALL_DIR/config.toml"
    echo ""
    echo "  3. Test it:"
    echo "     curl http://localhost:8765/health"
    echo ""
    echo "Documentation: https://github.com/$REPO#readme"
    echo ""
    info "Install complete. Add this to your shell profile:"
    echo ""
    echo "  export ORT_DYLIB_PATH=\"$NELLIE_LIB_DIR/libonnxruntime.so\""
    echo "  export NELLIE_ORT_VERSION=\"$ORT_VERSION\""
    echo ""
}

main "$@"
