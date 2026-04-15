#!/bin/bash
# Reset the nellie-test OrbStack VM to post-CC-login state.
# Preserves: Claude Code binary + auth credentials
# Removes: Rust, build tools, Nellie, ONNX Runtime, models, hooks, everything else
VM="nellie-test"
run() { orb run -m "$VM" bash -c "$1" || true; }

echo "=== Resetting nellie-test VM to clean state ==="

# Kill any running nellie processes
run 'pkill -f "nellie serve" 2>/dev/null; pkill -f "nellie" 2>/dev/null; true'

# Remove Nellie data, models, lib, config
run 'rm -rf ~/.local/share/nellie ~/.config/nellie /tmp/nellie*'

# Remove Nellie binary from PATH locations
run 'rm -f ~/.local/bin/nellie'

# Remove Claude Code hooks and injection files (preserve auth + settings structure)
run 'rm -rf ~/.claude/rules/nellie-* ; echo "{}" > ~/.claude/settings.json'

# Remove Rust toolchain
run 'rm -rf ~/.cargo ~/.rustup'
run 'sed -i "/\.cargo/d" ~/.bashrc; true'

# Remove build prerequisites (sudo is passwordless on OrbStack)
run 'sudo DEBIAN_FRONTEND=noninteractive apt-get remove -y --purge build-essential pkg-config libssl-dev libclang-dev clang cmake 2>/dev/null; sudo apt-get autoremove -y 2>/dev/null; sudo apt-get clean 2>/dev/null; true'

# Remove ONNX Runtime from system lib
run 'sudo rm -f /usr/local/lib/libonnxruntime*; sudo ldconfig 2>/dev/null; true'

# Remove any nellie-related PATH/env entries from .bashrc (preserve CC PATH)
run 'sed -i "/nellie/Id" ~/.bashrc; sed -i "/ORT_DYLIB_PATH/d" ~/.bashrc; sed -i "/NELLIE/d" ~/.bashrc; true'

# Remove build artifacts from mounted repo (if any were built on VM)
run 'rm -rf /Users/mmn/github/nellie/target'

# Verify clean state
echo ""
echo "=== Verification ==="
run 'echo "cargo: $(which cargo 2>/dev/null || echo NOT INSTALLED)"'
run 'echo "gcc:   $(which gcc 2>/dev/null || echo NOT INSTALLED)"'
run 'echo "nellie: $(which nellie 2>/dev/null || echo NOT INSTALLED)"'
run 'echo "claude: $(which claude 2>/dev/null || echo NOT INSTALLED)"'
run 'echo "CC auth: $(test -f ~/.claude/.credentials.json && echo PRESENT || echo MISSING)"'
run 'echo "Nellie data: $(test -d ~/.local/share/nellie && echo EXISTS || echo CLEAN)"'
run 'echo "Rust: $(test -d ~/.cargo && echo EXISTS || echo CLEAN)"'

echo ""
echo "=== VM reset complete. CC logged in, everything else clean. ==="
