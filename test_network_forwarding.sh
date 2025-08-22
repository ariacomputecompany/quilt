#\!/bin/bash

# Check network forwarding setup
echo "=== Checking network forwarding setup ==="

echo -e "\n1. IP forwarding:"
cat /proc/sys/net/ipv4/ip_forward

echo -e "\n2. Bridge interface:"
ip link show quilt0
ip addr show quilt0

echo -e "\n3. iptables rules:"
sudo iptables -t nat -L -n -v
echo "---"
sudo iptables -L FORWARD -n -v

echo -e "\n4. Bridge forwarding:"
cat /proc/sys/net/bridge/bridge-nf-call-iptables 2>/dev/null || echo "bridge-nf-call-iptables not set"

echo -e "\n5. Check if we need to add forwarding rules:"
echo "Current FORWARD policy:"
sudo iptables -L FORWARD -n | head -3
