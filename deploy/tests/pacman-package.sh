#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 PACKAGE EXPECTED_VERSION" >&2
  exit 2
fi

package_path="$(realpath "$1")"
expected_version="$2"

[[ -f "$package_path" ]] || {
  echo "package not found: $package_path" >&2
  exit 2
}

docker run --rm \
  --env "EXPECTED_VERSION=$expected_version" \
  --volume "$package_path:/tmp/barectl.pkg.tar.zst:ro" \
  archlinux:base \
  bash -euo pipefail -c '
    pacman -U --noconfirm /tmp/barectl.pkg.tar.zst
    [[ "$(pacman -Q barenetes-barectl)" == "barenetes-barectl ${EXPECTED_VERSION}-1" ]]
    [[ "$(barectl --version)" == "barectl ${EXPECTED_VERSION}" ]]
  '
