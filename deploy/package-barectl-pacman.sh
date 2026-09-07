#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 VERSION BARECTL_BINARY COMPLETIONS_DIR OUTPUT_DIR" >&2
  exit 2
fi

version="$1"
binary_path="$(realpath "$2")"
completions_dir="$(realpath "$3")"
output_dir="$(realpath -m "$4")"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "invalid version '$version': expected X.Y.Z" >&2
  exit 2
}
[[ -f "$binary_path" ]] || {
  echo "barectl binary not found: $binary_path" >&2
  exit 2
}
for completion in barectl.bash _barectl barectl.fish; do
  [[ -f "$completions_dir/$completion" ]] || {
    echo "completion file not found: $completions_dir/$completion" >&2
    exit 2
  }
done
command -v docker >/dev/null || {
  echo "docker is required to build the pacman package" >&2
  exit 2
}

staging_dir="$(mktemp -d)"
cleanup() {
  if [[ -n "${staging_dir:-}" && -d "$staging_dir" ]]; then
    rm -rf -- "$staging_dir"
  fi
}
trap cleanup EXIT

install -m 0644 "$script_dir/pacman/PKGBUILD" "$staging_dir/PKGBUILD"
sed -i "s/^pkgver=.*/pkgver=$version/" "$staging_dir/PKGBUILD"
install -m 0755 "$binary_path" "$staging_dir/barectl"
install -m 0644 "$completions_dir/barectl.bash" "$staging_dir/barectl.bash"
install -m 0644 "$completions_dir/_barectl" "$staging_dir/_barectl"
install -m 0644 "$completions_dir/barectl.fish" "$staging_dir/barectl.fish"
install -m 0644 "$script_dir/../LICENSE" "$staging_dir/LICENSE"

docker run --rm \
  --volume "$staging_dir:/build" \
  archlinux:base \
  bash -euo pipefail -c '
    pacman -Sy --noconfirm --needed fakeroot
    useradd --create-home builder
    chmod -R a+rwX /build
    runuser -u builder -- bash -c \
      "cd /build && makepkg --cleanbuild --noconfirm"
    chmod -R a+rwX /build
  '

mkdir -p "$output_dir"
install -m 0644 \
  "$staging_dir/barenetes-barectl-$version-1-x86_64.pkg.tar.zst" \
  "$output_dir/barenetes-barectl-$version-1-x86_64.pkg.tar.zst"
