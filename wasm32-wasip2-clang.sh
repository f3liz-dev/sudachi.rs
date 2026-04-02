#!/bin/sh
exec ~/Downloads/wasi-sdk-25.0-arm64-macos/bin/wasm32-wasip2-clang -fPIC "$@"
