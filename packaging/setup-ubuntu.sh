#!/bin/bash
# =============================================================================
# AI Dev Environment Setup for Ubuntu 25.04+
# Installs: OpenClaw, Claude Code, Nellie, DevPlan MCP
# =============================================================================

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

info() { echo -e "${GREEN}[✓]${NC} $1"; }
warn() { echo -e "${YELLOW}[!]${NC} $1"; }
error() { echo -e "${RED}[✗]${NC} $1"; }
step() { echo -e "${BLUE}[→]${NC} $1"; }

# Check if running as root
if [[ $EUID -eq 0 ]]; then
    error "Don't run this script as root. Run as your normal user."
    exit 1
fi

# =============================================================================
# Installation Functions
# =============================================================================

install_prerequisites() {
    step "Installing system prerequisites..."
    sudo apt update
    sudo apt install -y curl git build-essential
    
    # Check for Node.js 22+
    if command -v node &>/dev/null; then
        NODE_VERSION=$(node --version | cut -d'v' -f2 | cut -d'.' -f1)
        if [[ $NODE_VERSION -ge 22 ]]; then
            info "Node.js $(node --version) already installed"
            return 0
        fi
    fi
    
    step "Installing Node.js 22..."
    curl -fsSL https://deb.nodesource.com/setup_22.x | sudo -E bash -
    sudo apt install -y nodejs
    info "Node.js $(node --version) installed"
}

install_openclaw() {
    step "Installing OpenClaw..."
    npm install -g openclaw
    info "OpenClaw $(openclaw --version 2>/dev/null || echo 'installed')"
    
    if [[ ! -f ~/.openclaw/config.yaml ]]; then
        warn "Run 'openclaw init' to complete setup (API keys, channels)"
    fi
}

install_claude_code() {
    step "Installing Claude Code..."
    npm install -g @anthropic-ai/claude-code
    info "Claude Code installed"
    
    warn "Run 'claude' to complete first-time setup (API key)"
}

install_nellie() {
    step "Installing Nellie..."
    curl -sSL https://raw.githubusercontent.com/mmorris35/nellie/main/packaging/install-universal.sh | bash
    
    # Wait for service to start
    sleep 3
    
    if curl -s http://localhost:8765/health &>/dev/null; then
        info "Nellie running on http://localhost:8765"
    else
        warn "Nellie installed but may need configuration. Edit ~/.local/share/nellie/config.toml"
    fi
}

configure_watch_dirs() {
    echo ""
    echo -e "${CYAN}Configure Nellie Watch Directories${NC}"
    echo "Enter directories to watch (one per line, empty line when done):"
    echo "Example: /home/$USER/code"
    echo ""
    
    local dirs=()
    while true; do
        read -p "Directory: " dir
        [[ -z "$dir" ]] && break
        if [[ -d "$dir" ]]; then
            dirs+=("$dir")
            info "Added: $dir"
        else
            warn "Directory doesn't exist: $dir (skipping)"
        fi
    done
    
    if [[ ${#dirs[@]} -gt 0 ]]; then
        cat > ~/.local/share/nellie/config.toml << EOF
[server]
host = "127.0.0.1"
port = 8765

[watcher]
watch_dirs = [
$(printf '    "%s",\n' "${dirs[@]}")
]
EOF
        step "Restarting Nellie..."
        systemctl --user restart nellie
        sleep 2
        info "Watch directories configured"
    fi
}

setup_mcp_claude_code() {
    step "Configuring MCP servers for Claude Code..."
    
    # Nellie
    if curl -s http://localhost:8765/health &>/dev/null; then
        claude mcp add nellie --transport sse http://localhost:8765/sse --scope user 2>/dev/null || true
        info "Added Nellie MCP to Claude Code"
    else
        warn "Nellie not running, skipping Claude Code MCP setup"
    fi
    
    # DevPlan
    claude mcp add devplan --transport sse https://mcp.devplanmcp.store/sse --scope user 2>/dev/null || true
    info "Added DevPlan MCP to Claude Code"
    
    warn "Restart Claude Code for MCP changes to take effect"
}

setup_mcp_openclaw() {
    step "Configuring MCP servers for OpenClaw (mcporter)..."
    
    if ! command -v mcporter &>/dev/null; then
        step "Installing mcporter..."
        npm install -g mcporter
    fi
    
    # Nellie
    if curl -s http://localhost:8765/health &>/dev/null; then
        mcporter config add nellie --sse http://localhost:8765/sse 2>/dev/null || true
        info "Added Nellie MCP to mcporter"
    else
        warn "Nellie not running, skipping mcporter setup"
    fi
    
    # DevPlan
    mcporter config add devplan --sse https://mcp.devplanmcp.store/sse 2>/dev/null || true
    info "Added DevPlan MCP to mcporter"
}

enable_boot_persistence() {
    step "Enabling boot persistence (services start without login)..."
    sudo loginctl enable-linger $USER
    info "Lingering enabled for $USER"
}

verify_installation() {
    echo ""
    echo -e "${CYAN}=== Installation Status ===${NC}"
    echo ""
    
    # Node.js
    if command -v node &>/dev/null; then
        info "Node.js: $(node --version)"
    else
        error "Node.js: not installed"
    fi
    
    # OpenClaw
    if command -v openclaw &>/dev/null; then
        info "OpenClaw: installed"
    else
        error "OpenClaw: not installed"
    fi
    
    # Claude Code
    if command -v claude &>/dev/null; then
        info "Claude Code: installed"
    else
        error "Claude Code: not installed"
    fi
    
    # Nellie
    if curl -s http://localhost:8765/health &>/dev/null; then
        local stats=$(curl -s http://localhost:8765/health)
        info "Nellie: running (healthy)"
    else
        error "Nellie: not running"
    fi
    
    # mcporter
    if command -v mcporter &>/dev/null; then
        info "mcporter: installed"
    else
        warn "mcporter: not installed"
    fi
    
    echo ""
}

bootstrap_nellie() {
    echo ""
    echo -e "${CYAN}Nellie Bootstrap Instructions${NC}"
    echo ""
    echo "In Claude Code, paste this:"
    echo ""
    echo -e "${YELLOW}Use the Nellie MCP tool search_lessons to find \"How to Use Nellie\""
    echo -e "and follow the instructions. Then add Nellie usage instructions to"
    echo -e "~/.claude/CLAUDE.md so every future session uses Nellie automatically.${NC}"
    echo ""
    echo "In OpenClaw, paste this:"
    echo ""
    echo -e "${YELLOW}Search Nellie for the lesson \"How to Use Nellie\":"
    echo -e "mcporter call nellie.search_lessons query=\"how to use nellie for AI agents\""
    echo -e "Read the top result and add Nellie usage to AGENTS.md.${NC}"
    echo ""
}

install_all() {
    echo ""
    echo -e "${CYAN}=== Full Installation ===${NC}"
    echo ""
    
    install_prerequisites
    install_openclaw
    install_claude_code
    install_nellie
    configure_watch_dirs
    setup_mcp_claude_code
    setup_mcp_openclaw
    enable_boot_persistence
    verify_installation
    bootstrap_nellie
    
    echo -e "${GREEN}=== Installation Complete ===${NC}"
    echo ""
    echo "Next steps:"
    echo "  1. Run 'openclaw init' to configure OpenClaw"
    echo "  2. Run 'claude' to configure Claude Code"
    echo "  3. Follow the bootstrap instructions above"
    echo ""
}

# =============================================================================
# Menu
# =============================================================================

show_menu() {
    clear
    echo ""
    echo -e "${CYAN}╔═══════════════════════════════════════════════════════════╗${NC}"
    echo -e "${CYAN}║       AI Dev Environment Setup for Ubuntu 25.04+          ║${NC}"
    echo -e "${CYAN}╚═══════════════════════════════════════════════════════════╝${NC}"
    echo ""
    echo "  1) Install Everything (recommended)"
    echo ""
    echo "  Individual Components:"
    echo "  2) Install Prerequisites (Node.js, build tools)"
    echo "  3) Install OpenClaw"
    echo "  4) Install Claude Code"
    echo "  5) Install Nellie"
    echo ""
    echo "  Configuration:"
    echo "  6) Configure Nellie Watch Directories"
    echo "  7) Setup MCP for Claude Code (Nellie + DevPlan)"
    echo "  8) Setup MCP for OpenClaw (Nellie + DevPlan)"
    echo "  9) Enable Boot Persistence"
    echo ""
    echo "  Utilities:"
    echo "  v) Verify Installation Status"
    echo "  b) Show Nellie Bootstrap Instructions"
    echo "  q) Quit"
    echo ""
}

main() {
    while true; do
        show_menu
        read -p "Select option: " choice
        echo ""
        
        case $choice in
            1) install_all ;;
            2) install_prerequisites ;;
            3) install_openclaw ;;
            4) install_claude_code ;;
            5) install_nellie ;;
            6) configure_watch_dirs ;;
            7) setup_mcp_claude_code ;;
            8) setup_mcp_openclaw ;;
            9) enable_boot_persistence ;;
            v|V) verify_installation ;;
            b|B) bootstrap_nellie ;;
            q|Q) echo "Goodbye!"; exit 0 ;;
            *) warn "Invalid option" ;;
        esac
        
        echo ""
        read -p "Press Enter to continue..."
    done
}

# Run with menu by default, or specific function if passed
if [[ $# -eq 0 ]]; then
    main
else
    case $1 in
        --all) install_all ;;
        --prereq) install_prerequisites ;;
        --openclaw) install_openclaw ;;
        --claude) install_claude_code ;;
        --nellie) install_nellie ;;
        --verify) verify_installation ;;
        --help)
            echo "Usage: $0 [option]"
            echo ""
            echo "Options:"
            echo "  (none)      Interactive menu"
            echo "  --all       Install everything"
            echo "  --prereq    Install prerequisites only"
            echo "  --openclaw  Install OpenClaw only"
            echo "  --claude    Install Claude Code only"
            echo "  --nellie    Install Nellie only"
            echo "  --verify    Verify installation status"
            ;;
        *) error "Unknown option: $1"; exit 1 ;;
    esac
fi
