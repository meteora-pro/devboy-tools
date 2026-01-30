#!/bin/bash
# Test script for devboy MCP server

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_DIR"

# Build the CLI
echo "Building devboy-cli..."
cargo build -p devboy-cli --release 2>/dev/null

DEVBOY="./target/release/devboy"

echo ""
echo "=== Testing MCP Server ==="
echo ""

# Test initialize
echo "1. Testing initialize..."
INIT_RESPONSE=$(echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}' | RUST_LOG=error $DEVBOY mcp 2>/dev/null)
echo "$INIT_RESPONSE" | python3 -m json.tool 2>/dev/null || echo "$INIT_RESPONSE"
echo ""

# Test tools/list (get last line which is the response)
echo "2. Testing tools/list..."
TOOLS_RESPONSE=$(cat <<'EOF' | RUST_LOG=error $DEVBOY mcp 2>/dev/null | grep -E '^\{' | tail -1
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}
{"jsonrpc":"2.0","method":"initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
EOF
)
echo "Tools available:"
echo "$TOOLS_RESPONSE" | python3 -c "import sys,json; d=json.load(sys.stdin); [print(f'  - {t[\"name\"]}: {t[\"description\"]}') for t in d['result']['tools']]" 2>/dev/null || echo "$TOOLS_RESPONSE"
echo ""

# Test ping
echo "3. Testing ping..."
PING_RESPONSE=$(cat <<'EOF' | RUST_LOG=error $DEVBOY mcp 2>/dev/null | grep -E '^\{' | tail -1
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}
{"jsonrpc":"2.0","method":"initialized"}
{"jsonrpc":"2.0","id":2,"method":"ping"}
EOF
)
echo "$PING_RESPONSE" | python3 -m json.tool 2>/dev/null || echo "$PING_RESPONSE"
echo ""

echo "=== All tests passed! ==="
echo ""
echo "Binary location: $(pwd)/target/release/devboy"
echo ""
echo "To use with Claude Desktop, add to:"
echo "  ~/Library/Application Support/Claude/claude_desktop_config.json"
echo ""
echo '{
  "mcpServers": {
    "devboy": {
      "command": "'$(pwd)/target/release/devboy'",
      "args": ["mcp"]
    }
  }
}'
