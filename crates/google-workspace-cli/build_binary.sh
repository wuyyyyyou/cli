#!/bin/bash
# ============================================================
# gws-executa Binary Builder
# ============================================================
# Build the single-file Anna Executa plugin binary for gws.
#
# Usage:
#   ./build_binary.sh              # Build current platform in release mode
#   ./build_binary.sh --debug      # Build current platform in debug mode
#   ./build_binary.sh --all        # Attempt standard multi-platform builds
#   ./build_binary.sh --test       # Run protocol smoke tests after build
#
# Outputs:
#   dist/gws-executa-<platform>[-debug][.exe]
# ============================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

PACKAGE_NAME="google-workspace-cli"
BIN_NAME="gws-executa"
DIST_DIR="$SCRIPT_DIR/dist"
PROFILE="release"
BUILD_ALL=false
RUN_TEST=false

usage() {
    cat <<EOF
Usage: $0 [--debug] [--all] [--test]
  --debug   Build debug artifacts instead of release
  --all     Attempt standard multi-platform builds
  --test    Run protocol smoke tests against the host artifact
  --help    Show this help
EOF
}

for arg in "$@"; do
    case "$arg" in
        --debug) PROFILE="debug" ;;
        --all) BUILD_ALL=true ;;
        --test) RUN_TEST=true ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo -e "${RED}Unknown option: $arg${NC}"
            usage
            exit 1
            ;;
    esac
done

host_triple() {
    rustc -vV | awk '/^host:/ {print $2}'
}

platform_key_from_triple() {
    case "$1" in
        aarch64-apple-darwin) echo "darwin-arm64" ;;
        x86_64-apple-darwin) echo "darwin-x86_64" ;;
        x86_64-unknown-linux-gnu) echo "linux-x86_64" ;;
        aarch64-unknown-linux-gnu) echo "linux-aarch64" ;;
        x86_64-pc-windows-gnu|x86_64-pc-windows-msvc) echo "windows-x86_64" ;;
        aarch64-pc-windows-msvc) echo "windows-arm64" ;;
        *) echo "$1" ;;
    esac
}

binary_suffix_for_triple() {
    case "$1" in
        *windows*) echo ".exe" ;;
        *) echo "" ;;
    esac
}

profile_output_dir() {
    if [[ "$PROFILE" == "release" ]]; then
        echo "release"
    else
        echo "debug"
    fi
}

build_target() {
    local target="$1"
    local platform_key="$2"
    local suffix="$3"
    local artifact_dir
    artifact_dir="$(profile_output_dir)"
    local source_path="target/${target}/${artifact_dir}/${BIN_NAME}${suffix}"
    local output_name="${BIN_NAME}-${platform_key}"
    if [[ "$PROFILE" == "debug" ]]; then
        output_name="${output_name}-debug"
    fi
    output_name="${output_name}${suffix}"
    local output_path="${DIST_DIR}/${output_name}"

    echo -e "${GREEN}Building ${platform_key} (${target})...${NC}"
    rustup target add "$target" >/dev/null
    if [[ "$PROFILE" == "release" ]]; then
        cargo build -p "$PACKAGE_NAME" --bin "$BIN_NAME" --release --target "$target"
    else
        cargo build -p "$PACKAGE_NAME" --bin "$BIN_NAME" --target "$target"
    fi

    cp "$source_path" "$output_path"
    chmod +x "$output_path" 2>/dev/null || true
    echo -e "  ${CYAN}→${NC} ${output_path}"
}

run_protocol_tests() {
    local binary="$1"

    echo ""
    echo -e "${GREEN}Running protocol smoke tests...${NC}"

    local describe
    describe="$(echo '{"jsonrpc":"2.0","method":"describe","id":1}' | "$binary" 2>/dev/null)"
    if ! echo "$describe" | jq -e '.result.name == "gws-executa"' >/dev/null; then
        echo -e "${RED}describe test failed${NC}"
        exit 1
    fi
    echo -e "  ${GREEN}✓ describe${NC}"

    local health
    health="$(echo '{"jsonrpc":"2.0","method":"health","id":2}' | "$binary" 2>/dev/null)"
    if ! echo "$health" | jq -e '.result.status == "healthy"' >/dev/null; then
        echo -e "${RED}health test failed${NC}"
        exit 1
    fi
    echo -e "  ${GREEN}✓ health${NC}"

    local invoke_pointer
    invoke_pointer="$(echo '{"jsonrpc":"2.0","method":"invoke","id":3,"params":{"tool":"run_gws","arguments":{"argv":["--version"]},"context":{"credentials":{"GOOGLE_WORKSPACE_CLI_TOKEN":"dummy-token"}}}}' | "$binary" 2>/dev/null)"
    local invoke_file
    invoke_file="$(echo "$invoke_pointer" | jq -r '."__file_transport"')"
    if [[ -z "$invoke_file" || "$invoke_file" == "null" || ! -f "$invoke_file" ]]; then
        echo -e "${RED}invoke test failed: file transport pointer missing${NC}"
        exit 1
    fi
    local invoke
    invoke="$(cat "$invoke_file")"
    rm -f "$invoke_file"
    if ! echo "$invoke" | jq -e '.result.tool == "run_gws" and .result.data.exit_code == 0' >/dev/null; then
        echo -e "${RED}invoke test failed${NC}"
        exit 1
    fi
    echo -e "  ${GREEN}✓ invoke (--version, file transport success path)${NC}"
}

mkdir -p "$DIST_DIR"

HOST_TRIPLE="$(host_triple)"
HOST_PLATFORM_KEY="$(platform_key_from_triple "$HOST_TRIPLE")"

echo -e "${CYAN}============================================================${NC}"
echo -e "${CYAN}  gws-executa Binary Builder${NC}"
echo -e "${CYAN}============================================================${NC}"
echo -e "  Package:  ${PACKAGE_NAME}"
echo -e "  Binary:   ${BIN_NAME}"
echo -e "  Host:     ${HOST_TRIPLE}"
echo -e "  Profile:  ${PROFILE}"
echo -e "  Output:   ${DIST_DIR}"
echo ""

rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

declare -a TARGETS
if [[ "$BUILD_ALL" == "true" ]]; then
    TARGETS=(
        "aarch64-apple-darwin"
        "x86_64-apple-darwin"
        "x86_64-unknown-linux-gnu"
        "aarch64-unknown-linux-gnu"
        "x86_64-pc-windows-gnu"
    )
else
    TARGETS=("$HOST_TRIPLE")
fi

declare -a BUILT_OUTPUTS=()
declare -a FAILED_TARGETS=()

for target in "${TARGETS[@]}"; do
    platform_key="$(platform_key_from_triple "$target")"
    suffix="$(binary_suffix_for_triple "$target")"
    if build_target "$target" "$platform_key" "$suffix"; then
        output_name="${BIN_NAME}-${platform_key}"
        if [[ "$PROFILE" == "debug" ]]; then
            output_name="${output_name}-debug"
        fi
        BUILT_OUTPUTS+=("${DIST_DIR}/${output_name}${suffix}")
    else
        FAILED_TARGETS+=("$target")
        echo -e "${YELLOW}Skipping failed target: ${target}${NC}"
    fi
done

echo ""
echo -e "${GREEN}Build complete.${NC}"
for artifact in "${BUILT_OUTPUTS[@]}"; do
    size="$(du -h "$artifact" | cut -f1)"
    echo -e "  ${CYAN}•${NC} ${artifact} (${size})"
done

if [[ ${#FAILED_TARGETS[@]} -gt 0 ]]; then
    echo ""
    echo -e "${YELLOW}Some targets failed to build.${NC}"
    for failed in "${FAILED_TARGETS[@]}"; do
        echo -e "  ${YELLOW}•${NC} ${failed}"
    done
    echo -e "${YELLOW}Cross-platform builds may require additional system linkers or toolchains.${NC}"
fi

if [[ "$RUN_TEST" == "true" ]]; then
    host_suffix="$(binary_suffix_for_triple "$HOST_TRIPLE")"
    host_output="${DIST_DIR}/${BIN_NAME}-${HOST_PLATFORM_KEY}"
    if [[ "$PROFILE" == "debug" ]]; then
        host_output="${host_output}-debug"
    fi
    host_output="${host_output}${host_suffix}"
    run_protocol_tests "$host_output"
fi

echo ""
echo -e "${CYAN}Next step:${NC}"
echo -e "  Use the artifact under ${DIST_DIR} as the Anna plugin binary."
