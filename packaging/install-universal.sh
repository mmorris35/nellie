#!/bin/bash
# Nellie-RS Universal Installer
# Usage: curl -sSL https://github.com/mmorris35/nellie/releases/latest/download/install-universal.sh | bash
#
# Or with specific version:
# curl -sSL https://github.com/mmorris35/nellie/releases/download/v0.1.0/install-universal.sh | bash

set -euo pipefail

REPO="mmorris35/nellie"
INSTALL_DIR="${NELLIE_INSTALL_DIR:-$HOME/.nellie-rs}"
BIN_DIR="${NELLIE_BIN_DIR:-$HOME/.local/bin}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info() { echo -e "${GREEN}==>${NC} $1"; }
warn() { echo -e "${YELLOW}Warning:${NC} $1"; }
error() { echo -e "${RED}Error:${NC} $1" >&2; exit 1; }

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

# Get latest release version
get_latest_version() {
    curl -sSL "https://api.github.com/repos/$REPO/releases/latest" | \
        grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/'
}

# Download binary
download_binary() {
    local artifact="$1"
    local version="$2"
    local url="https://github.com/$REPO/releases/download/$version/$artifact"
    
    info "Downloading $artifact ($version)..."
    curl -sSL -o "$INSTALL_DIR/nellie" "$url" || error "Failed to download from $url"
    chmod +x "$INSTALL_DIR/nellie"
}

# Download embedding model
download_model() {
    local model_dir="$INSTALL_DIR/models"
    local model_file="$model_dir/all-MiniLM-L6-v2.onnx"
    
    if [[ -f "$model_file" ]]; then
        info "Embedding model already exists"
        return
    fi
    
    info "Downloading embedding model (this may take a moment)..."
    mkdir -p "$model_dir"
    
    # Download from Hugging Face
    curl -sSL -o "$model_file" \
        "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx" \
        || error "Failed to download embedding model"
    
    info "Model downloaded to $model_file"
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
# Nellie-RS Configuration
# Edit this file to customize your Nellie instance

[server]
host = "127.0.0.1"
port = 8765

[storage]
# Database location (default: ~/.nellie-rs/nellie.db)
# db_path = "/path/to/nellie.db"

[embeddings]
# Model path (default: ~/.nellie-rs/models/all-MiniLM-L6-v2.onnx)
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
        echo "# Nellie-RS" >> "$shell_rc"
        echo "$path_line" >> "$shell_rc"
        info "Added $BIN_DIR to PATH in $shell_rc"
        warn "Run 'source $shell_rc' or restart your terminal"
    fi
}

# Create launchd plist for macOS
setup_macos_service() {
    local plist_dir="$HOME/Library/LaunchAgents"
    local plist_file="$plist_dir/com.nellie-rs.plist"
    
    mkdir -p "$plist_dir"
    
    cat > "$plist_file" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.nellie-rs</string>
    <key>ProgramArguments</key>
    <array>
        <string>$INSTALL_DIR/nellie</string>
        <string>--config</string>
        <string>$INSTALL_DIR/config.toml</string>
    </array>
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
Description=Nellie-RS Code Memory Server
After=network.target

[Service]
Type=simple
ExecStart=$INSTALL_DIR/nellie --config $INSTALL_DIR/config.toml
WorkingDirectory=$INSTALL_DIR
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
}

# Main installation
main() {
    echo ""
    echo "╔═══════════════════════════════════════════╗"
    echo "║     Nellie-RS Installer                   ║"
    echo "║     Your AI-Powered Code Memory           ║"
    echo "╚═══════════════════════════════════════════╝"
    echo ""
    
    local artifact version
    artifact="$(detect_platform)"
    version="$(get_latest_version)"
    
    if [[ -z "$version" ]]; then
        error "Could not determine latest version. Check your internet connection."
    fi
    
    info "Platform: $artifact"
    info "Version: $version"
    echo ""
    
    # Create install directory
    mkdir -p "$INSTALL_DIR/logs"
    
    # Download and install
    download_binary "$artifact" "$version"
    download_model
    create_config
    setup_shell
    
    # Setup service based on OS
    echo ""
    if [[ "$(uname -s)" == "Darwin" ]]; then
        setup_macos_service
    else
        setup_linux_service
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
}

main "$@"
