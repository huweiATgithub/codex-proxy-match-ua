"""Validate plugin metadata and assemble the exact assets verified on the released hosts."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-unknown-linux-gnu"


def read_json(path):
    return json.loads(path.read_text())


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def metadata(tag):
    manifest = read_json(ROOT / "plugin.json")
    crate, = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--locked", "--format-version", "1"], cwd=ROOT))["packages"]
    version = manifest["version"]
    if version != crate["version"] or (tag and tag != f"v{version}"):
        raise ValueError("Git tag, plugin.json and Cargo.toml must identify the same release")
    sdk, = (dep for dep in crate["dependencies"] if dep["name"] == "gateway-plugin-sdk")
    pin = re.fullmatch(r"git\+https://github\.com/zyycn/codex-proxy-rs\?rev=([0-9a-f]{40})", sdk["source"])
    if not pin:
        raise ValueError("the SDK and packager require an exact upstream revision")
    hosts = read_json(ROOT / "host-compatibility.json")["hosts"]
    host_range = manifest["engines"]["codex-proxy-rs"]
    bounds = re.fullmatch(r">=(\d+\.\d+\.\d+), <(\d+\.\d+\.\d+)", host_range)
    if not hosts or not bounds or hosts[0]["version"] != bounds[1]:
        raise ValueError("the first integration host must exercise the declared minimum version")
    previous = ()
    for host in hosts:
        version_tuple = tuple(map(int, host["version"].split(".")))
        if not (previous < version_tuple < tuple(map(int, bounds[2].split(".")))):
            raise ValueError("verification hosts must be distinct, ordered and within the engine range")
        if not re.fullmatch(r"[0-9a-f]{40}", host["revision"]) or not re.fullmatch(r"[0-9a-f]{64}", host["sha256"]):
            raise ValueError("each verification host needs a pinned commit and binary archive checksum")
        previous = version_tuple
    return {"version": version, "sdk-revision": pin[1], "host-range": host_range,
            "archive": f"{manifest['publisher']}.{manifest['name']}-{version}-{TARGET}.tar.gz",
            "matrix": json.dumps({"include": hosts}, separators=(",", ":"))}


def check_elf(path):
    header = path.read_bytes()[:20]
    assert header[:6] == b"\x7fELF\x02\x01" and int.from_bytes(header[18:20], "little") == 62, "expected x86-64 ELF"
    info = subprocess.check_output(["readelf", "--version-info", str(path)], text=True)
    needed = max(tuple(map(int, v.split("."))) for v in re.findall(r"GLIBC_(\d+(?:\.\d+)+)", info))
    assert needed <= (2, 34), f"glibc requirement exceeds 2.34: {needed}"
    print("Maximum required glibc", ".".join(map(str, needed)))


def prepare(directory, evidence, archive_name):
    archive = directory / archive_name
    digest = sha256(archive)
    checksum = (directory / (archive_name + ".sha256")).read_text().split()
    if checksum != [digest, archive_name]:
        raise ValueError("the package checksum does not match the release archive")
    results = []
    for host in read_json(ROOT / "host-compatibility.json")["hosts"]:
        result = read_json(evidence / (host["version"] + ".json"))
        if (result["hostVersion"], result["hostRevision"], result["packageSha256"]) != (
                host["version"], host["revision"], digest) or not result["checks"]:
            raise ValueError(f"host {host['version']} did not verify this package")
        results.append(result)
    report = {"archive": archive_name, "sha256": digest, "hosts": results}
    (directory / "verification.json").write_text(json.dumps(report, indent=2) + "\n")
    shutil.copyfile(ROOT / "LICENSE", directory / "LICENSE")
    names = sorted([archive_name, archive_name + ".sha256", "LICENSE", "verification.json"])
    (directory / "SHA256SUMS").write_text("".join(f"{sha256(directory / n)}  {n}\n" for n in names))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    meta = commands.add_parser("metadata")
    meta.add_argument("--tag", default="")
    elf = commands.add_parser("check-elf")
    elf.add_argument("path", type=Path)
    assets = commands.add_parser("prepare")
    assets.add_argument("directory", type=Path)
    assets.add_argument("evidence", type=Path)
    assets.add_argument("archive")
    args = parser.parse_args()
    if args.command == "check-elf":
        check_elf(args.path)
    elif args.command == "prepare":
        prepare(args.directory, args.evidence, args.archive)
    else:
        values = metadata(args.tag)
        print(json.dumps(values, indent=2))
        if "GITHUB_OUTPUT" in os.environ:
            with open(os.environ["GITHUB_OUTPUT"], "a") as output:
                for name, value in values.items():
                    print(f"{name}={value}", file=output)
