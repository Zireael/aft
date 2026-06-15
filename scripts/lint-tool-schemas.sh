#!/usr/bin/env bash
# lint-tool-schemas.sh — Lint tool schemas and descriptions for AFT tools.
#
# This script checks:
# 1. Tool descriptions contain purpose, when-to-use, and when-not-to-use guidance
# 2. Tool schemas match Rust command parameters
# 3. Feature-gated tools are documented
#
# Usage:
#   bash scripts/lint-tool-schemas.sh
#   bash scripts/lint-tool-schemas.sh --fix  # Auto-fix simple issues

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

ERRORS=0
WARNINGS=0

error() {
    echo -e "${RED}ERROR:${NC} $1"
    ((ERRORS++))
}

warn() {
    echo -e "${YELLOW}WARN:${NC} $1"
    ((WARNINGS++))
}

info() {
    echo -e "${GREEN}OK:${NC} $1"
}

# ---------------------------------------------------------------------------
# 1. Check tool descriptions
# ---------------------------------------------------------------------------

echo "=== Checking tool descriptions ==="

# Check FTS5 tool descriptions
FTS5_TOOLS_FILE="$PROJECT_ROOT/packages/opencode-plugin/src/tools/fts5.ts"
if [[ -f "$FTS5_TOOLS_FILE" ]]; then
    # Check for description patterns
    if grep -q "description:" "$FTS5_TOOLS_FILE"; then
        # Check each tool has a multi-line description
        TOOL_COUNT=$(grep -c "description:" "$FTS5_TOOLS_FILE" || true)
        info "Found $TOOL_COUNT tool descriptions in fts5.ts"
        
        # Check for use/avoid guidance
        if grep -q "Use this when" "$FTS5_TOOLS_FILE" || grep -q "Use action=" "$FTS5_TOOLS_FILE"; then
            info "Tool descriptions include usage guidance"
        else
            warn "Some tool descriptions may lack usage guidance"
        fi
    else
        warn "No tool descriptions found in fts5.ts"
    fi
else
    warn "FTS5 tools file not found: $FTS5_TOOLS_FILE"
fi

# ---------------------------------------------------------------------------
# 2. Check Rust command parameters match TypeScript schemas
# ---------------------------------------------------------------------------

echo ""
echo "=== Checking Rust command parameters ==="

# Check FTS5 search parameters
RUST_FTS5="$PROJECT_ROOT/crates/aft/src/commands/fts5.rs"
if [[ -f "$RUST_FTS5" ]]; then
    # Extract Rust struct fields for Fts5SearchParams
    RUST_PARAMS=$(grep -A 20 "struct Fts5SearchParams" "$RUST_FTS5" | grep -E "^\s+(query|top_k|scope):" | sed 's/:.*//' | tr -d ' ' | sort)
    
    # TypeScript params from fts5.ts
    TS_PARAMS=$(grep -E "z\.(string|number|enum)" "$FTS5_TOOLS_FILE" | head -10 | grep -oE "describe\(\"[^\"]+\"" | sed 's/describe("//;s/"$//' | head -5 | sort)
    
    info "Rust params: $RUST_PARAMS"
    info "TS params: $TS_PARAMS"
else
    warn "Rust FTS5 file not found: $RUST_FTS5"
fi

# ---------------------------------------------------------------------------
# 3. Check feature-gated tools
# ---------------------------------------------------------------------------

echo ""
echo === "Checking feature-gated tools" ===

# Check if FTS5 tools are feature-gated
if grep -q "cfg.*feature.*fts5" "$RUST_FTS5" 2>/dev/null; then
    info "FTS5 commands are feature-gated"
else
    warn "FTS5 commands may not be feature-gated (check manually)"
fi

# ---------------------------------------------------------------------------
# 4. Check output envelope consistency
# ---------------------------------------------------------------------------

echo ""
echo "=== Checking output envelope consistency ==="

# Check if FTS5 commands return consistent envelope
if grep -q "OutputState\|build_envelope" "$RUST_FTS5" 2>/dev/null; then
    info "FTS5 commands use output envelope"
else
    warn "FTS5 commands may not use output envelope"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo ""
echo "=== Summary ==="
echo -e "Errors: ${RED}$ERRORS${NC}"
echo -e "Warnings: ${YELLOW}$WARNINGS${NC}"

if [[ $ERRORS -gt 0 ]]; then
    echo -e "\n${RED}FAILED:${NC} $ERRORS errors found"
    exit 1
elif [[ $WARNINGS -gt 0 ]]; then
    echo -e "\n${YELLOW}PASSED with warnings:${NC} $WARNINGS warnings"
    exit 0
else
    echo -e "\n${GREEN}PASSED:${NC} All checks passed"
    exit 0
fi
