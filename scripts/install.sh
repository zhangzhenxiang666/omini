#!/usr/bin/env sh
set -eu

repository="zhangzhenxiang666/omini"
version="${OMINI_VERSION:-latest}"

os="$(uname -s)"
arch="$(uname -m)"
case "$os/$arch" in
  Darwin/arm64)
    target="aarch64-apple-darwin"
    ;;
  Linux/x86_64)
    target="x86_64-unknown-linux-gnu"
    ;;
  *)
    echo "omini supports macOS Apple Silicon and Linux x86_64; detected $os/$arch" >&2
    exit 1
    ;;
esac

case "$version" in
  latest) release_base="https://github.com/$repository/releases/latest/download" ;;
  *) release_base="https://github.com/$repository/releases/download/v$version" ;;
esac

archive="omini-$target.tar.gz"
temp_dir="$(mktemp -d)"
cleanup() { rm -rf "$temp_dir"; }
trap cleanup EXIT HUP INT TERM

curl -fsSL "$release_base/$archive" -o "$temp_dir/$archive"
curl -fsSL "$release_base/SHA256SUMS" -o "$temp_dir/SHA256SUMS"

expected="$(awk -v asset="$archive" '$2 == asset { print $1 }' "$temp_dir/SHA256SUMS")"
if [ -z "$expected" ]; then
  echo "release checksum missing for $archive" >&2
  exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$temp_dir/$archive" | awk '{ print $1 }')"
else
  actual="$(shasum -a 256 "$temp_dir/$archive" | awk '{ print $1 }')"
fi
if [ "$actual" != "$expected" ]; then
  echo "checksum mismatch for $archive" >&2
  exit 1
fi

tar -xzf "$temp_dir/$archive" -C "$temp_dir"
for binary in omini omini-server rg; do
  if [ ! -f "$temp_dir/bin/$binary" ]; then
    echo "release archive is missing bin/$binary" >&2
    exit 1
  fi
done

user_bin="${HOME}/.local/bin"
internal_bin="${HOME}/.omini/bin"
mkdir -p "$user_bin" "$internal_bin"
install -m 0755 "$temp_dir/bin/omini" "$user_bin/omini"
install -m 0755 "$temp_dir/bin/omini-server" "$internal_bin/omini-server"
install -m 0755 "$temp_dir/bin/rg" "$internal_bin/rg"

echo "Installed omini to $user_bin/omini"
if ! command -v omini >/dev/null 2>&1; then
  echo "Add $user_bin to PATH to run omini." >&2
fi
