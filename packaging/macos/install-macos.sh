#!/bin/bash
# Nellie-RS macOS Installation Script
# For deployment to Mac Mini (mini-dev-server)

set -e

NELLIE_BIN="/usr/local/bin/nellie"
NELLIE_DATA="/var/lib/nellie-rs"
NELLIE_CONFIG="/usr/local/etc/nellie"
LAUNCHD_PLIST="/Library/LaunchDaemons/com.nellie-rs.server.plist"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "=== Nellie-RS macOS Installation ==="
echo ""

# Check if running as root for system-wide installation
if [[ $EUID -ne 0 ]]; then
    echo "Note: Running without sudo. Some operations may require elevated privileges."
    echo "Run with sudo for full system installation."
    echo ""
fi

# Check for binary
if [[ ! -f "$SCRIPT_DIR/../../target/release/nellie" ]]; then
    echo "Error: Release binary not found."
    echo "Build first with: cargo build --release"
    exit 1
fi

# Verify it's an ARM64 binary on macOS
if [[ "$(uname)" == "Darwin" ]]; then
    ARCH=$(file "$SCRIPT_DIR/../../target/release/nellie" | grep -o 'arm64\|x86_64')
    echo "Binary architecture: $ARCH"
    if [[ "$(uname -m)" == "arm64" && "$ARCH" != "arm64" ]]; then
        echo "Warning: Binary is $ARCH but system is ARM64. Rebuild natively for best performance."
    fi
fi

echo ""
echo "Step 1: Installing binary to $NELLIE_BIN"
sudo cp "$SCRIPT_DIR/../../target/release/nellie" "$NELLIE_BIN"
sudo chmod +x "$NELLIE_BIN"
echo "  Done."

echo ""
echo "Step 2: Creating data directory $NELLIE_DATA"
sudo mkdir -p "$NELLIE_DATA/logs"
sudo chown -R "$(whoami)" "$NELLIE_DATA"
echo "  Done."

echo ""
echo "Step 3: Creating config directory $NELLIE_CONFIG"
sudo mkdir -p "$NELLIE_CONFIG"
echo "  Done."

echo ""
echo "Step 4: Installing launchd plist"
if [[ -f "$LAUNCHD_PLIST" ]]; then
    echo "  Unloading existing service..."
    sudo launchctl unload "$LAUNCHD_PLIST" 2>/dev/null || true
fi
sudo cp "$SCRIPT_DIR/com.nellie-rs.server.plist" "$LAUNCHD_PLIST"
sudo chown root:wheel "$LAUNCHD_PLIST"
sudo chmod 644 "$LAUNCHD_PLIST"
echo "  Done."

echo ""
echo "Step 5: Verifying installation"
"$NELLIE_BIN" --version
echo "  Binary OK."

echo ""
echo "=== Installation Complete ==="
echo ""
echo "Next steps:"
echo "  1. Start the service:"
echo "     sudo launchctl load $LAUNCHD_PLIST"
echo ""
echo "  2. Check status:"
echo "     sudo launchctl list | grep nellie"
echo ""
echo "  3. View logs:"
echo "     tail -f $NELLIE_DATA/logs/nellie.out.log"
echo ""
echo "  4. Test health:"
echo "     curl http://localhost:8766/health"
echo ""
echo "  5. Run migration (if migrating from Python Nellie):"
echo "     $SCRIPT_DIR/migrate-from-python.sh"
echo ""
