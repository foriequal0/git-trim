#!/usr/bin/env bash

MODE=$1
case $MODE in
build)
    cargo build --bin build-man --features build-man
    ;;
run|"")
    mkdir -p docs/
    cargo run --bin build-man --features build-man > docs/git-trim.1
    MANWIDTH=120 man docs/git-trim.1 > docs/git-trim.man
    ;;
*)
    echo "Unknown mode: $MODE"
    exit -1
    ;;
esac
