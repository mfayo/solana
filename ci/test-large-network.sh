#!/bin/bash -e

here=$(dirname "$0")
cd "$here"/..

if ! ci/version-check.sh stable; then
  # This job doesn't run within a container, try once to upgrade tooling on a
  # version check failure
  rustup install stable
  ci/version-check.sh stable
fi
export RUST_BACKTRACE=1

./fetch-perf-libs.sh
export LD_LIBRARY_PATH+=:$PWD

export RUST_LOG=multinode=info

ulimit -Hn
ulimit -Sn
ulimit -Ha
ulimit -Sa

set -x
exec cargo test --release --features=erasure test_multi_node_dynamic_network -- --ignored
