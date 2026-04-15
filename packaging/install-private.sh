#!/bin/bash
# Nellie-RS Private Repo Installer
# Requires: gh CLI authenticated with repo access
#
# Usage: 
#   gh release download v0.1.0 --repo mmorris35/nellie-rs -p "install-private.sh" -O - | bash

set -euo pipefail

REPO="mmorris35/nellie-rs"
VERSION="${NELLIE_VERSION:-latest}"
INSTALL_DIR="${NELLIE_INSTALL_DIR:-$HOME/.nellie-rs}"
BIN_DIR="${NELLIE_BIN_DIR:-$HOME/.local/bin}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info() { echo -e "${GREEN}==>${NC} $1"; }
warn() { echo -e "${YELLOW}Warning:${NC} $1"; }
error() { echo -e "${RED}Error:${NC} $1" >&2; exit 1; }

# Check gh CLI
check_gh() {
    if ! command -v gh &> /dev/null; then
        error "gh CLI not found. Install from: https://cli.github.com"
    fi
    
    if ! gh auth status &> /dev/null; then
        error "Not authenticated. Run: gh auth login"
    fi
    
    info "GitHub CLI authenticated ✓"
}

# Detect platform
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

# Download binary via gh
download_binary() {
    local artifact="$1"
    
    info "Downloading $artifact..."
    mkdir -p "$INSTALL_DIR"
    
    if [[ "$VERSION" == "latest" ]]; then
        gh release download --repo "$REPO" -p "$artifact" -O "$INSTALL_DIR/nellie" --clobber
    else
        gh release download "$VERSION" --repo "$REPO" -p "$artifact" -O "$INSTALL_DIR/nellie" --clobber
    fi
    
    chmod +x "$INSTALL_DIR/nellie"
    info "Binary installed to $INSTALL_DIR/nellie"
}

# Download model
download_model() {
    local model_dir="$INSTALL_DIR/models"
    local model_file="$model_dir/all-MiniLM-L6-v2.onnx"
    
    if [[ -f "$model_file" ]]; then
        info "Embedding model already exists"
        return
    fi
    
    info "Downloading embedding model..."
    mkdir -p "$model_dir"
    curl -sSL -o "$model_file" \
        "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx"
    info "Model downloaded"
}

# Create config
create_config() {
    local config_file="$INSTALL_DIR/config.toml"
    
    if [[ -f "$config_file" ]]; then
        info "Config exists at $config_file"
        return
    fi
    
    info "Creating config..."
    cat > "$config_file" << 'EOF'
# Nellie-RS Configuration

[server]
host = "127.0.0.1"
port = 8765

[watcher]
# Add your code directories here:
# watch_dirs = [
#     "/path/to/your/code",
# ]
EOF
    warn "Edit $config_file to add your watch directories!"
}

# Setup PATH
setup_path() {
    mkdir -p "$BIN_DIR"
    ln -sf "$INSTALL_DIR/nellie" "$BIN_DIR/nellie"
    
    local shell_rc=""
    case "$SHELL" in
        */zsh)  shell_rc="$HOME/.zshrc" ;;
        */bash) shell_rc="$HOME/.bashrc" ;;
    esac
    
    if [[ -n "$shell_rc" ]] && ! grep -q "$BIN_DIR" "$shell_rc" 2>/dev/null; then
        echo -e "\n# Nellie-RS\nexport PATH=\"$BIN_DIR:\$PATH\"" >> "$shell_rc"
        warn "Added to PATH. Run: source $shell_rc"
    fi
}

# Setup service
setup_service() {
    mkdir -p "$INSTALL_DIR/logs"
    
    if [[ "$(uname -s)" == "Darwin" ]]; then
        local plist="$HOME/Library/LaunchAgents/com.nellie-rs.plist"
        mkdir -p "$(dirname "$plist")"
        cat > "$plist" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>com.nellie-rs</string>
    <key>ProgramArguments</key>
    <array>
        <string>$INSTALL_DIR/nellie</string>
        <string>--config</string>
        <string>$INSTALL_DIR/config.toml</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>$INSTALL_DIR/logs/nellie.log</string>
    <key>StandardErrorPath</key><string>$INSTALL_DIR/logs/nellie.log</string>
</dict>
</plist>
EOF
        info "Created: $plist"
        echo ""
        echo "Start Nellie:"
        echo "  launchctl load $plist"
    else
        local service="$HOME/.config/systemd/user/nellie.service"
        mkdir -p "$(dirname "$service")"
        cat > "$service" << EOF
[Unit]
Description=Nellie-RS
After=network.target
[Service]
ExecStart=$INSTALL_DIR/nellie --config $INSTALL_DIR/config.toml
Restart=on-failure
[Install]
WantedBy=default.target
EOF
        info "Created: $service"
        echo ""
        echo "Start Nellie:"
        echo "  systemctl --user enable --now nellie"
    fi
}

main() {
    echo ""
    echo "╔═══════════════════════════════════════╗"
    echo "║  Nellie-RS Private Installer          ║"
    echo "╚═══════════════════════════════════════╝"
    echo ""
    
    check_gh
    
    local artifact
    artifact="$(detect_platform)"
    info "Platform: $artifact"
    
    download_binary "$artifact"
    download_model
    create_config
    setup_path
    setup_service
    
    echo ""
    echo "════════════════════════════════════════"
    info "Done! Edit $INSTALL_DIR/config.toml then start the service."
    echo ""
}

main "$@"
