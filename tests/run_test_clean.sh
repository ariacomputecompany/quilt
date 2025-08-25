#!/bin/bash
# Run test with clean output
cargo build --quiet 2>/dev/null
./tests/test_icc_comprehensive.sh 2>&1 | grep -v "warning:"