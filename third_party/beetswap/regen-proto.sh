#!/bin/bash
set -e -x

if ! command -v pb-rs > /dev/null 2>&1; then
    echo "Run: cargo install pb-rs" >&2
    exit 1
fi

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
pb-rs -D -s ./src/proto/message.proto
