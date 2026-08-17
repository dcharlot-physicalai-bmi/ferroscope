#!/usr/bin/env bash
# Rebuild the browser viewer's wasm bundle. One command, no bundler, no node_modules.
#
#   ./viewer/build.sh
#
# Then serve the directory over http (a module import of a .wasm cannot come from file://):
#
#   python3 -m http.server 8080 --directory viewer
#   open http://localhost:8080
set -euo pipefail
cd "$(dirname "$0")/.."
# --out-dir is resolved RELATIVE TO THE CRATE PATH, not the working directory. Passing
# "viewer/pkg" wrote the bundle to crates/ferroscope-wasm/viewer/pkg and left the real one
# untouched, which also made CI's "committed bundle matches source" check unable to fail.
wasm-pack build --release --target web --no-typescript \
  --out-dir "$PWD/viewer/pkg" crates/ferroscope-wasm
rm -f viewer/pkg/.gitignore          # the pkg IS committed, so the viewer works from a clone
ls -l viewer/pkg
