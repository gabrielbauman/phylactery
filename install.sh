#!/usr/bin/env bash
set -euo pipefail

# Phylactery install script
# Builds from source, installs binaries, and optionally initializes the agent home.

BINARIES=(
    phyl
    phylactd
    phyl-run
    phyl-model-claude
    phyl-model-openai
    phyl-tool-bash
    phyl-tool-files
    phyl-tool-session
    phyl-tool-mcp
    phyl-bridge-signal
    phyl-poll
    phyl-listen
)

INSTALL_DIR="${HOME}/.local/bin"
SKIP_INIT=false
MIN_RUST_MAJOR=1
MIN_RUST_MINOR=85

# --- Colors ---

if [[ -t 1 ]] && [[ -z "${NO_COLOR:-}" ]]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    BOLD='\033[1m'
    RESET='\033[0m'
else
    RED=''
    GREEN=''
    YELLOW=''
    BOLD=''
    RESET=''
fi

# --- Helpers ---

info()  { printf "${GREEN}==> %s${RESET}\n" "$*"; }
warn()  { printf "${YELLOW}warning:${RESET} %s\n" "$*" >&2; }
error() { printf "${RED}error:${RESET} %s\n" "$*" >&2; }
die()   { error "$@"; exit 1; }

usage() {
    cat <<EOF
Usage: ./install.sh [OPTIONS]

Build and install phylactery from source.

Options:
    --install-dir <path>    Install binaries to <path> (default: ~/.local/bin)
    --skip-init             Don't run 'phyl init' after installing
    -h, --help              Show this help message
EOF
    exit 0
}

# --- Argument parsing ---

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --install-dir)
                [[ -n "${2:-}" ]] || die "--install-dir requires a path argument"
                INSTALL_DIR="$2"
                shift 2
                ;;
            --skip-init)
                SKIP_INIT=true
                shift
                ;;
            -h|--help)
                usage
                ;;
            *)
                die "Unknown option: $1 (see --help)"
                ;;
        esac
    done
}

# --- Prerequisites ---

check_prerequisites() {
    info "Checking prerequisites"

    if ! command -v git &>/dev/null; then
        die "git is not installed. Install it from https://git-scm.com/"
    fi

    if ! command -v rustc &>/dev/null; then
        die "rustc is not installed. Install Rust via https://rustup.rs/"
    fi

    if ! command -v cargo &>/dev/null; then
        die "cargo is not installed. Install Rust via https://rustup.rs/"
    fi

    local rust_version
    rust_version="$(rustc --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')"
    local major minor
    major="$(echo "$rust_version" | cut -d. -f1)"
    minor="$(echo "$rust_version" | cut -d. -f2)"

    if [[ "$major" -lt "$MIN_RUST_MAJOR" ]] || \
       { [[ "$major" -eq "$MIN_RUST_MAJOR" ]] && [[ "$minor" -lt "$MIN_RUST_MINOR" ]]; }; then
        die "Rust ${MIN_RUST_MAJOR}.${MIN_RUST_MINOR}+ required (found ${rust_version}). Run: rustup update stable"
    fi

    printf "  git: %s\n" "$(git --version)"
    printf "  rustc: %s\n" "$rust_version"
}

# --- Platform detection ---

detect_platform() {
    local uname_s
    uname_s="$(uname -s)"
    case "$uname_s" in
        Linux)  PLATFORM="linux" ;;
        Darwin) PLATFORM="macos" ;;
        *)
            warn "Unsupported platform: ${uname_s}. Proceeding anyway."
            PLATFORM="unknown"
            ;;
    esac
    printf "  platform: %s\n" "$PLATFORM"
}

# --- Build ---

build() {
    info "Building (cargo build --release)"

    local repo_root
    repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    cd "$repo_root"

    cargo build --release

    local missing=()
    for bin in "${BINARIES[@]}"; do
        if [[ ! -f "target/release/${bin}" ]]; then
            missing+=("$bin")
        fi
    done

    if [[ ${#missing[@]} -gt 0 ]]; then
        die "Build succeeded but missing binaries: ${missing[*]}"
    fi

    printf "  built %d binaries\n" "${#BINARIES[@]}"
}

# --- Install ---

install_binaries() {
    info "Installing binaries to ${INSTALL_DIR}"

    mkdir -p "$INSTALL_DIR"

    for bin in "${BINARIES[@]}"; do
        cp "target/release/${bin}" "${INSTALL_DIR}/${bin}"
    done

    printf "  installed %d binaries\n" "${#BINARIES[@]}"

    # Check if install dir is on PATH
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            warn "${INSTALL_DIR} is not on your PATH."
            echo
            echo "Add it by appending this to your shell profile:"
            echo
            if [[ "${SHELL:-}" == */zsh ]]; then
                echo "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.zshrc"
                echo "  source ~/.zshrc"
            elif [[ "${SHELL:-}" == */bash ]]; then
                echo "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.bashrc"
                echo "  source ~/.bashrc"
            else
                echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
            fi
            echo
            ;;
    esac
}

# --- Agent home detection (mirrors phyl-core home_dir logic) ---

find_existing_home() {
    # 1. Explicit env var
    if [[ -n "${PHYLACTERY_HOME:-}" ]] && [[ -d "$PHYLACTERY_HOME" ]]; then
        echo "$PHYLACTERY_HOME"
        return 0
    fi

    # 2. XDG_DATA_HOME
    if [[ -n "${XDG_DATA_HOME:-}" ]]; then
        local xdg_path="${XDG_DATA_HOME}/phylactery"
        if [[ -d "$xdg_path" ]]; then
            echo "$xdg_path"
            return 0
        fi
    fi

    # 3. Platform-specific data dir
    if [[ "$PLATFORM" == "macos" ]]; then
        local mac_path="${HOME}/Library/Application Support/phylactery"
        if [[ -d "$mac_path" ]]; then
            echo "$mac_path"
            return 0
        fi
    else
        local linux_path="${HOME}/.local/share/phylactery"
        if [[ -d "$linux_path" ]]; then
            echo "$linux_path"
            return 0
        fi
    fi

    # 4. Legacy path
    local legacy_path="${HOME}/.phylactery"
    if [[ -d "$legacy_path" ]]; then
        echo "$legacy_path"
        return 0
    fi

    return 1
}

init_agent_home() {
    if [[ "$SKIP_INIT" == true ]]; then
        info "Skipping init (--skip-init)"
        return
    fi

    local existing_home
    if existing_home="$(find_existing_home)"; then
        info "Agent home already exists at ${existing_home} — skipping init"
        return
    fi

    info "Initializing agent home (phyl init)"
    "${INSTALL_DIR}/phyl" init
}

# --- Post-install checks ---

post_install_checks() {
    info "Checking optional dependencies"

    if ! command -v claude &>/dev/null; then
        warn "'claude' CLI not found on PATH."
        echo "  phyl-model-claude requires the Anthropic claude CLI."
        echo "  Install it from: https://docs.anthropic.com/en/docs/claude-cli"
        echo "  Alternatively, use phyl-model-openai with a local model server."
        echo
    fi
}

# --- Next steps ---

print_next_steps() {
    local services_cmd
    if [[ "$PLATFORM" == "macos" ]]; then
        services_cmd="launchd"
    else
        services_cmd="systemd"
    fi

    echo
    printf "${BOLD}Installation complete.${RESET}\n"
    echo
    echo "Next steps:"
    echo "  phyl config edit                  # Edit LAW.md, JOB.md, config.toml"
    echo "  phyl config add mcp ...           # Add tool servers"
    printf "  phyl setup %-20s  # Install as %s user services\n" "$services_cmd" "$services_cmd"
    echo "  phyl start                        # Or just start the daemon"
    echo
}

# --- Main ---

main() {
    parse_args "$@"
    check_prerequisites
    detect_platform
    build
    install_binaries
    init_agent_home
    post_install_checks
    print_next_steps
}

main "$@"
