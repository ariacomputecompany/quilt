#!/bin/bash

echo "🔍 Bridge Context Debugging"
echo "=========================="

echo "1. Direct command:"
ip link show quilt0

echo ""
echo "2. CommandExecutor equivalent:"
/bin/sh -c "ip link show quilt0"

echo ""
echo "3. With explicit error handling:"
if /bin/sh -c "ip link show quilt0" 2>/dev/null; then
    echo "✅ Bridge found with error redirection"
else
    echo "❌ Bridge not found with error redirection"
fi

echo ""
echo "4. Test stderr capture:"
result=$(/bin/sh -c "ip link show quilt0" 2>&1)
echo "Result: '$result'"

echo ""
echo "5. Test command that should fail:"
/bin/sh -c "ip link show nonexistent" 2>&1 || echo "Command failed as expected" 