#!/bin/bash
set -e

echo "=== Testing New Quilt Features ==="
echo

# Colors for output
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

# Build the project
echo -e "${BLUE}Building Quilt...${NC}"
cargo build --release --bin quilt --bin cli
echo

# Start the server in background
echo -e "${BLUE}Starting Quilt server...${NC}"
./target/release/quilt &
SERVER_PID=$!
sleep 2

# Function to cleanup on exit
cleanup() {
    echo -e "\n${YELLOW}Cleaning up...${NC}"
    # Kill all containers
    for id in $(./target/release/cli status 2>/dev/null | grep -E "^Container ID:" | awk '{print $3}'); do
        ./target/release/cli kill $id 2>/dev/null || true
        ./target/release/cli remove $id --force 2>/dev/null || true
    done
    # Kill server
    kill $SERVER_PID 2>/dev/null || true
    wait $SERVER_PID 2>/dev/null || true
}
trap cleanup EXIT

# Generate test image if needed
if [ ! -f "nixos-minimal.tar.gz" ]; then
    echo -e "${BLUE}Generating test container image...${NC}"
    ./dev.sh generate-rootfs
fi

echo -e "\n${GREEN}=== Test 1: Create container with name ===${NC}"
./target/release/cli create -n test-container --image-path nixos-minimal.tar.gz -- echo "Hello from named container"
sleep 1

echo -e "\n${GREEN}=== Test 2: Get status by name ===${NC}"
./target/release/cli status test-container -n

echo -e "\n${GREEN}=== Test 3: Create async container ===${NC}"
./target/release/cli create -n async-test --async-mode --image-path nixos-minimal.tar.gz
sleep 1

echo -e "\n${GREEN}=== Test 4: List all containers ===${NC}"
./target/release/cli status async-test -n

echo -e "\n${GREEN}=== Test 5: Execute command in container by name ===${NC}"
./target/release/cli exec async-test -n -c "echo 'Hello from exec'" -c "pwd" --capture-output

echo -e "\n${GREEN}=== Test 6: Create a test script and execute it ===${NC}"
cat > /tmp/test_script.sh << 'EOF'
#!/bin/sh
echo "Running test script inside container"
echo "Current directory: $(pwd)"
echo "Hostname: $(hostname)"
echo "Process list:"
ps aux
EOF
chmod +x /tmp/test_script.sh

./target/release/cli exec async-test -n -c /tmp/test_script.sh --capture-output

echo -e "\n${GREEN}=== Test 7: Stop container gracefully ===${NC}"
./target/release/cli stop async-test -n -t 5

echo -e "\n${GREEN}=== Test 8: Start stopped container ===${NC}"
./target/release/cli start async-test -n
sleep 1

echo -e "\n${GREEN}=== Test 9: Kill container immediately ===${NC}"
./target/release/cli kill async-test -n

echo -e "\n${GREEN}=== Test 10: Remove container by name ===${NC}"
./target/release/cli remove async-test -n

echo -e "\n${GREEN}=== Test 11: Create multiple containers with names ===${NC}"
./target/release/cli create -n web-server --async-mode --image-path nixos-minimal.tar.gz
./target/release/cli create -n database --async-mode --image-path nixos-minimal.tar.gz
./target/release/cli create -n cache --async-mode --image-path nixos-minimal.tar.gz
sleep 1

echo -e "\n${GREEN}=== Test 12: Test ICC - Ping between containers ===${NC}"
./target/release/cli icc ping web-server database --count 3

echo -e "\n${GREEN}=== Test 13: Execute commands across containers ===${NC}"
./target/release/cli exec web-server -n -c "echo 'Web server is running'" --capture-output
./target/release/cli exec database -n -c "echo 'Database is running'" --capture-output
./target/release/cli exec cache -n -c "echo 'Cache is running'" --capture-output

echo -e "\n${GREEN}=== Test 14: Cleanup all containers ===${NC}"
./target/release/cli kill web-server -n
./target/release/cli kill database -n
./target/release/cli kill cache -n
./target/release/cli remove web-server -n
./target/release/cli remove database -n
./target/release/cli remove cache -n

echo -e "\n${GREEN}=== Test 15: Test error handling - duplicate names ===${NC}"
./target/release/cli create -n duplicate --async-mode --image-path nixos-minimal.tar.gz
if ./target/release/cli create -n duplicate --async-mode --image-path nixos-minimal.tar.gz 2>&1 | grep -q "already exists"; then
    echo -e "${GREEN}✓ Duplicate name correctly rejected${NC}"
else
    echo -e "${RED}✗ Duplicate name was not rejected${NC}"
fi
./target/release/cli kill duplicate -n
./target/release/cli remove duplicate -n

echo -e "\n${GREEN}All tests completed successfully!${NC}"