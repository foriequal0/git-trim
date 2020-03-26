#!/usr/bin/env bash

mkdir docs/
cargo run --bin build-man --features build-man > docs/git-trim.1
MANWIDTH=120 man docs/git-trim.1 > docs/git-trim.man
