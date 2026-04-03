#!/bin/bash
set -euo pipefail

# Nellie Production Installation Script
# Usage: sudo ./install.sh [binary_path]

BINARY_PATH="${1:-./target/release/nellie}"
INSTALL_DIR="/usr/local/bin"
DATA_DIR="/var/lib/nellie"
LOG_DIR="/var/log/nellie"
CONFIG_DIR="/etc/nellie"
USER="nellie"
GROUP="nellie"

echo "Installing Nellie Production..."

# Check root
if [[ $EUID -ne 0 ]]; then
   echo "This script must be run as root (use sudo)"
   exit 1
fi

# Check binary exists
if [[ ! -f "$BINARY_PATH" ]]; then
    echo "Error: Binary not found at $BINARY_PATH"
    echo "Build with: cargo build --release"
    exit 1
fi

# Create user and group
if ! id -u "$USER" &>/dev/null; then
    echo "Creating user: $USER"
    useradd --system --shell /usr/sbin/nologin --home-dir "$DATA_DIR" "$USER"
fi

# Create directories
echo "Creating directories..."
mkdir -p "$DATA_DIR"
mkdir -p "$DATA_DIR/models"
mkdir -p "$LOG_DIR"
mkdir -p "$CONFIG_DIR"

# Install binary
echo "Installing binary to $INSTALL_DIR..."
cp "$BINARY_PATH" "$INSTALL_DIR/nellie"
chmod 755 "$INSTALL_DIR/nellie"

# Install service file
echo "Installing systemd service..."
cp packaging/nellie.service /etc/systemd/system/
chmod 644 /etc/systemd/system/nellie.service

# Install config if not exists
if [[ ! -f "$CONFIG_DIR/nellie.conf" ]]; then
    echo "Installing default configuration..."
    cp packaging/nellie.conf "$CONFIG_DIR/"
    chmod 640 "$CONFIG_DIR/nellie.conf"
    chown root:$GROUP "$CONFIG_DIR/nellie.conf"
fi

# Set permissions
echo "Setting permissions..."
chown -R "$USER:$GROUP" "$DATA_DIR"
chown -R "$USER:$GROUP" "$LOG_DIR"
chmod 750 "$DATA_DIR"
chmod 750 "$LOG_DIR"

# Reload systemd
echo "Reloading systemd..."
systemctl daemon-reload

# Print next steps
echo ""
echo "Installation complete!"
echo ""
echo "Next steps:"
echo "  1. Download ONNX model to $DATA_DIR/models/"
echo "     wget -O $DATA_DIR/models/all-MiniLM-L6-v2.onnx <model-url>"
echo "  2. Edit configuration: $CONFIG_DIR/nellie.conf"
echo "  3. Start service: systemctl start nellie"
echo "  4. Enable on boot: systemctl enable nellie"
echo "  5. Check status: systemctl status nellie"
echo "  6. View logs: journalctl -u nellie -f"
echo ""
