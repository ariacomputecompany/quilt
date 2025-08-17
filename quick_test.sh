#\!/bin/bash
set -e

# Start server
./target/debug/quilt > /dev/null 2>&1 &
SERVER_PID=$\!
sleep 2

# Create async container
echo "Creating async container..."
./target/debug/cli create -n test-async --async-mode --image-path nixos-minimal.tar.gz
sleep 2

# Check status
echo "Checking status..."
./target/debug/cli status test-async -n

# Stop it
echo "Stopping container..."
./target/debug/cli stop test-async -n
sleep 1

# Try to start it again
echo "Starting container again..."
./target/debug/cli start test-async -n

# Check status again
echo "Checking status after start..."
./target/debug/cli status test-async -n

# Cleanup
./target/debug/cli kill test-async -n 2>/dev/null || true
./target/debug/cli remove test-async -n --force 2>/dev/null || true
kill $SERVER_PID 2>/dev/null || true

echo "Test completed\!"
