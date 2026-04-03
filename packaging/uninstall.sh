#!/bin/bash
set -euo pipefail

# Nellie Production Uninstallation Script
# Usage: sudo ./uninstall.sh

echo "Uninstalling Nellie Production..."

# Check root
if [[ $EUID -ne 0 ]]; then
   echo "This script must be run as root (use sudo)"
   exit 1
fi

# Stop service if running
if systemctl is-active --quiet nellie; then
    echo "Stopping service..."
    systemctl stop nellie
fi

# Disable service
if systemctl is-enabled --quiet nellie 2>/dev/null; then
    echo "Disabling service..."
    systemctl disable nellie
fi

# Remove service file
echo "Removing service file..."
rm -f /etc/systemd/system/nellie.service
systemctl daemon-reload

# Remove binary
echo "Removing binary..."
rm -f /usr/local/bin/nellie

echo ""
echo "Uninstallation complete!"
echo ""
echo "The following were NOT removed (manual cleanup if needed):"
echo "  - Data directory: /var/lib/nellie"
echo "  - Log directory: /var/log/nellie"
echo "  - Config directory: /etc/nellie"
echo "  - User: nellie"
echo ""
