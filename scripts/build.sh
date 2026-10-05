#!/bin/sh
# Prepare the plugin binary before herdr moves its temporary installation checkout.
set -eu

install_launcher=0
case "$#" in
    0) ;;
    1)
        if [ "$1" = "--install-launcher" ]; then
            install_launcher=1
        else
            printf 'usage: sh scripts/build.sh [--install-launcher]\n' >&2
            exit 2
        fi
        ;;
    *)
        printf 'usage: sh scripts/build.sh [--install-launcher]\n' >&2
        exit 2
        ;;
esac

script_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd)
cd "$script_dir/.."
cargo build --release --locked --bins --target-dir target
mkdir -p bin
stage_path="bin/.herdr-crew.$$.tmp"
trap 'rm -f "$stage_path"' 0
trap 'exit 1' HUP INT TERM
install -m 755 target/release/herdr-crew "$stage_path"
mv -f "$stage_path" bin/herdr-crew

if [ "$install_launcher" -eq 1 ]; then
    target/release/herdr-crew-launcher --install-launcher
fi
printf 'Prepared bin/herdr-crew\n'
