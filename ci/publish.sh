#!/usr/bin/env bash
set -euo pipefail

: "${RELEASE_TAG:?}" "${HOST_RANGE:?}" "${SDK_REVISION:?}" "${GH_REPO:?}" "${RUNNER_TEMP:?}"
assets=$(realpath dist/release)
scratch=$(mktemp -d "$RUNNER_TEMP/match-ua-release.XXXXXX")
python3 - "$assets/verification.json" "$scratch/notes.md" <<'PY'
import json, os, pathlib, sys
verified = json.loads(pathlib.Path(sys.argv[1]).read_text())
versions = ", ".join("v" + h["hostVersion"] for h in verified["hosts"])
pathlib.Path(sys.argv[2]).write_text(
    "This plugin rejects Codex requests when the client and effective key User-Agents report different operating systems.\n\n"
    "Linux x86-64 requires glibc 2.34 or newer.\n\n"
    f"Host range is `{os.environ['HOST_RANGE']}`. This archive passed integration tests on {versions}. "
    f"The SDK and packaging CLI use `{os.environ['SDK_REVISION']}`.\n\n"
    "Install the plugin archive from the proxy administration page. "
    "The verification report includes each tested host revision and package checksum. "
    "Run `sha256sum --check SHA256SUMS` in the download directory to verify the assets.\n")
PY

if draft=$(gh release view "$RELEASE_TAG" --json isDraft --jq .isDraft); then
  :
else
  gh release create "$RELEASE_TAG" --verify-tag --draft --title "$RELEASE_TAG" --notes-file "$scratch/notes.md"
  draft=true
fi
if [[ "$draft" == true ]]; then
  gh release upload "$RELEASE_TAG" "$assets"/* --clobber
fi
gh release download "$RELEASE_TAG" --dir "$scratch/staged"
diff --recursive "$assets" "$scratch/staged"
(cd "$scratch/staged" && sha256sum --check --strict SHA256SUMS)
if [[ "$draft" == true ]]; then
  gh release edit "$RELEASE_TAG" --draft=false
fi
mkdir "$scratch/public"
for asset in "$assets"/*; do
  name=$(basename "$asset")
  curl --fail --location --silent --show-error --retry 5 --retry-all-errors \
    "$GITHUB_SERVER_URL/$GITHUB_REPOSITORY/releases/download/$RELEASE_TAG/$name" --output "$scratch/public/$name"
done
diff --recursive "$assets" "$scratch/public"
(cd "$scratch/public" && sha256sum --check --strict SHA256SUMS)
