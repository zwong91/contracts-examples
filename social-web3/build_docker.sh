#!/usr/bin/env bash

# Exit script as soon as a command fails.
set -e

DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

NAME="build_social_db"

if docker ps -a --format '{{.Names}}' | grep -Eq "^${NAME}\$"; then
    echo "Container exists"
else
docker create \
     --mount type=bind,source=$DIR,target=/host \
     --cap-add=SYS_PTRACE --security-opt seccomp=unconfined \
     --name=$NAME \
     -w /host \
     -e RUSTFLAGS='-C link-arg=-s' \
     -it \
     unc/contract-builder \
     /bin/bash
fi

docker start $NAME
docker exec -it $NAME /bin/bash -c "rustup toolchain install 1.77.2; rustup default 1.77.2; rustup target add wasm32-unknown-unknown; cargo build --package contract --target wasm32-unknown-unknown --release"

mkdir -p res
cp $DIR/target/wasm32-unknown-unknown/release/contract.wasm $DIR/res/social_web3_release.wasm

