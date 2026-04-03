#!/bin/bash
# Nellie-RS Local Installer
# Put this script in the same folder as the nellie binary, then run it.
#
# Usage: ./install-local.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
INSTALL_DIR="${NELLIE_INSTALL_DIR:-$HOME/.nellie-rs}"
BIN_DIR="${NELLIE_BIN_DIR:-$HOME/.local/bin}"

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

info() { echo -e "${GREEN}==>${NC} $1"; }
warn() { echo -e "${YELLOW}Warning:${NC} $1"; }
error() { echo -e "${RED}Error:${NC} $1" >&2; exit 1; }

# Find binary
find_binary() {
    local os arch binary
    os="$(uname -s)"
    arch="$(uname -m)"
    
    case "$os" in
        Darwin) os="macos" ;;
        Linux)  os="linux" ;;
        *)      error "Unsupported OS: $os" ;;
    esac
    
    case "$arch" in
        x86_64|amd64)  arch="x86_64" ;;
        arm64|aarch64) arch="aarch64" ;;
        *)             error "Unsupported arch: $arch" ;;
    esac
    
    binary="nellie-${os}-${arch}"
    
    # Try exact match first
    if [[ -f "$SCRIPT_DIR/$binary" ]]; then
        echo "$SCRIPT_DIR/$binary"
        return
    fi
    
    # Try just "nellie"
    if [[ -f "$SCRIPT_DIR/nellie" ]]; then
        echo "$SCRIPT_DIR/nellie"
        return
    fi
    
    error "Binary not found. Expected: $binary or nellie in $SCRIPT_DIR"
}

main() {
    echo ""
    echo "╔═══════════════════════════════════════╗"
    echo "║  Nellie-RS Local Installer            ║"
    echo "╚═══════════════════════════════════════╝"
    echo ""
    
    local binary
    binary="$(find_binary)"
    info "Found binary: $(basename "$binary")"
    
    # Create directories
    mkdir -p "$INSTALL_DIR/logs"
    mkdir -p "$INSTALL_DIR/models"
    mkdir -p "$BIN_DIR"
    
    # Copy binary
    info "Installing to $INSTALL_DIR/nellie"
    cp "$binary" "$INSTALL_DIR/nellie"
    chmod +x "$INSTALL_DIR/nellie"
    ln -sf "$INSTALL_DIR/nellie" "$BIN_DIR/nellie"
    
    # Download model if needed
    local model="$INSTALL_DIR/models/all-MiniLM-L6-v2.onnx"
    if [[ ! -f "$model" ]]; then
        info "Downloading embedding model..."
        curl -sSL -o "$model" \
            "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx"
    else
        info "Model already exists"
    fi
    
    # Create config if needed
    local config="$INSTALL_DIR/config.toml"
    if [[ ! -f "$config" ]]; then
        info "Creating config..."
        cat > "$config" << 'EOF'
# Nellie-RS Configuration

[server]
host = "127.0.0.1"
port = 8765

[watcher]
# Add your code directories:
watch_dirs = [
    # "/Users/yourname/code",
    # "/Users/yourname/projects",
]
EOF
    fi
    
    # Setup shell PATH
    local shell_rc=""
    case "$SHELL" in
        */zsh)  shell_rc="$HOME/.zshrc" ;;
        */bash) shell_rc="$HOME/.bashrc" ;;
    esac
    
    if [[ -n "$shell_rc" ]] && ! grep -q "$BIN_DIR" "$shell_rc" 2>/dev/null; then
        echo -e "\n# Nellie-RS\nexport PATH=\"$BIN_DIR:\$PATH\"" >> "$shell_rc"
        info "Added to PATH in $shell_rc"
    fi
    
    # Create launchd plist (macOS)
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
        info "Created launchd service"
        
        # Start the service
        launchctl unload "$plist" 2>/dev/null || true
        launchctl load "$plist"
        info "Started Nellie service"
    fi
    
    # Create systemd service (Linux)
    if [[ "$(uname -s)" == "Linux" ]]; then
        local service="$HOME/.config/systemd/user/nellie.service"
        mkdir -p "$(dirname "$service")"
        cat > "$service" << EOF
[Unit]
Description=Nellie-RS Code Memory
After=network.target

[Service]
ExecStart=$INSTALL_DIR/nellie --config $INSTALL_DIR/config.toml
Restart=on-failure

[Install]
WantedBy=default.target
EOF
        info "Created systemd user service"
        
        # Start the service
        systemctl --user daemon-reload
        systemctl --user enable nellie
        systemctl --user restart nellie
        info "Started Nellie service"
    fi
    
    echo ""
    echo "════════════════════════════════════════"
    echo ""
    info "Installation complete!"
    echo ""
    # Wait a moment for service to start
    sleep 2
    
    # Check if it's running
    if curl -s http://localhost:8765/health >/dev/null 2>&1; then
        echo "  ${GREEN}✓ Nellie is running on http://localhost:8765${NC}"
        echo ""
        echo "Next steps:"
        echo ""
        echo "  1. Edit your config to add watch directories:"
        echo "     ${YELLOW}nano $config${NC}"
        echo ""
        echo "  2. Restart after config changes:"
        if [[ "$(uname -s)" == "Darwin" ]]; then
            echo "     ${YELLOW}launchctl unload ~/Library/LaunchAgents/com.nellie-rs.plist${NC}"
            echo "     ${YELLOW}launchctl load ~/Library/LaunchAgents/com.nellie-rs.plist${NC}"
        else
            echo "     ${YELLOW}systemctl --user restart nellie${NC}"
        fi
    else
        warn "Service may not have started. Check logs:"
        echo "     ${YELLOW}cat $INSTALL_DIR/logs/nellie.log${NC}"
        echo ""
        echo "To start manually:"
        if [[ "$(uname -s)" == "Darwin" ]]; then
            echo "     ${YELLOW}launchctl load ~/Library/LaunchAgents/com.nellie-rs.plist${NC}"
        else
            echo "     ${YELLOW}systemctl --user start nellie${NC}"
        fi
    fi
    echo ""
}

main "$@"
