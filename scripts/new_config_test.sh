#!/bin/sh
set -e

if test -z "$1"; then
	based_on=$(find ./data/configs -type f -print0 | sort -z | tail -n 1 -z)
else
	based_on="$1"
fi

new="./data/configs/$(date -u -Iseconds).json"
cp "$based_on" "$new"

$EDITOR "$new"
