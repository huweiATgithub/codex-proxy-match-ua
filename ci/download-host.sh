#!/usr/bin/env bash
set -euo pipefail

version=${1:?host version}
revision=${2:?host commit}
checksum=${3:?archive digest}
destination=${4:?destination directory}
repository=zyycn/codex-proxy-rs
archive="codex-proxy-rs_${version}_linux_amd64.tar.gz"
actual=$(gh api "repos/$repository/commits/v$version" --jq .sha)
[[ "$actual" == "$revision" ]] || { echo "Host v$version points to $actual, expected $revision" >&2; exit 1; }
mkdir -p "$destination"
gh release download "v$version" --repo "$repository" --pattern "$archive" --dir "$destination" --clobber
(
  cd "$destination"
  echo "$checksum  $archive" | sha256sum --check --strict
  tar -xzf "$archive"
)
