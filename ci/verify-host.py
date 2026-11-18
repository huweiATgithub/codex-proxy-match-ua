"""Exercise the packaged plugin on one released host and write release evidence."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import time

from host_fixture import run_host, wait

ROOT = Path(__file__).resolve().parents[1]
MATRIX = json.loads((ROOT / "tests/ua-matrix-0.167.2.json").read_text())["platforms"]
UBUNTU = MATRIX["linux-ubuntu-arm64"]["profiles"]["WindowsTerminal"]["CLI"]
WINDOWS = MATRIX["windows-x64"]["profiles"]["WindowsTerminal"]["CLI"]


def preset(platform):
    return {"client": "cli", "platform": platform, "versionMode": "fixed",
            "codexVersion": "0.167.2", "cliEntry": "exec"}


def set_global(host, platform):
    settings = host.admin("/api/admin/settings")
    for output_only in ("smartSchedulingDefaults", "quotaResetWindowWeightsDefaults", "updatedAt",
                        "updatedAtDisplay", "openaiClientProfile", "xaiClientProfile"):
        settings.pop(output_only, None)
    settings["providerRequestProfiles"] = {"openai": preset(platform)}
    host.admin("/api/admin/settings/update", settings)


def create_key(host, name, profile=None):
    config = {"name": name, "groupIds": [], "maxConcurrency": 0, "requestsPerMinute": 0}
    if profile is not None:
        config["providerRequestProfileOverrides"] = {"openai": profile}
    return host.admin("/api/admin/client-keys/create", config)


def send(host, key, ua, expected, family):
    before = len(host.records)
    status, headers, body = host.request("/v1/responses", {
        "model": "gpt-5.4", "stream": False, "store": False, "input": [{"role": "user", "content": "test"}],
    }, {"Authorization": "Bearer " + key["plaintextKey"], "User-Agent": ua})
    assert status == expected, (status, body)
    if status == 200:
        assert body["id"].startswith("resp_test_")
        assert len(host.records) == before + 1 and f"({family} " in host.records[-1], host.records[before:]
    else:
        assert len(host.records) == before, "rejected request reached upstream"
        assert body["error"]["code"] == "policy_denied", body
        assert body["error"]["message"].startswith("UA incompatible."), body
        assert f"requires {family}." in body["error"]["message"], body
    return headers["x-gateway-request-id"]


def verify_storage(host, instance, request_id, key, second_id):
    path = host.directory / "runtime" / "huwei.match-ua" / instance / "rejections.sqlite3"

    def recorded():
        assert path.is_file(), f"waiting for {path.name}"
        with sqlite3.connect(path) as db:
            row = db.execute("SELECT client_key_id, client_ua, client_platform, effective_platform, reason "
                             "FROM rejected_requests WHERE request_id=?", (request_id,)).fetchone()
            assert row == (key["id"], UBUNTU, "Linux", "Windows", "platform_mismatch"), row
            assert db.execute("SELECT count(*) FROM rejected_requests WHERE request_id=?", (second_id,)).fetchone()[0]
    wait(recorded)
    with sqlite3.connect(path) as db:
        db.execute("BEGIN IMMEDIATE")
        started = time.monotonic()
        blocked_id = send(host, key, UBUNTU, 403, "Windows")
        latency = round((time.monotonic() - started) * 1000)
        assert not db.execute("SELECT 1 FROM rejected_requests WHERE request_id=?", (blocked_id,)).fetchone()
        db.rollback()
        now = int(time.time())
        db.execute("UPDATE rejected_requests SET rejected_at=? WHERE request_id=?", (now - 8 * 86400, request_id))
        db.execute("UPDATE rejected_requests SET rejected_at=? WHERE request_id=?", (now - 6 * 86400, second_id))
        db.commit()

        def swept():
            recent = db.execute("SELECT request_id FROM rejected_requests WHERE request_id IN (?,?)",
                                (request_id, second_id)).fetchall()
            assert recent == [(second_id,)], "waiting for the host maintenance callback"
        wait(swept, timeout=75)
    return latency


def verify_page(host, instance, request_id, key):
    subprocess.run(["node", str(ROOT / "tests/management-page.mjs")], check=True, timeout=60, env={
        **os.environ, "CPR_TEST_URL": host.url, "CPR_TEST_COOKIE": host.cookie, "CPR_TEST_INSTANCE": instance,
        "CPR_TEST_REQUEST_ID": request_id, "CPR_TEST_KEY_ID": key["id"], "CPR_TEST_CLIENT_UA": UBUNTU,
    })


def verify(release, archive):
    payload = archive.read_bytes()
    digest = hashlib.sha256(payload).hexdigest()
    with run_host(release) as host:
        upload = host.admin("/api/admin/plugins/artifacts/upload?sha256=" + digest, payload)
        instance = upload["defaultInstanceId"]

        def enabled():
            assert any(row["id"] == instance and row["enabled"] for row in host.admin("/api/admin/plugins/instances"))
        wait(enabled)
        print("Installed package", digest, flush=True)
        host.admin("/api/admin/accounts/import", {"provider": "openai", "data": {
            "authentication_kind": "api_key", "name": "loopback", "base_url": host.upstream,
            "api_key": "sk-loopback-fixture", "transport": "http"}})
        set_global(host, "linux")
        inherited = create_key(host, "inherited")
        windows = create_key(host, "windows", preset("windows"))
        send(host, inherited, UBUNTU, 200, "Linux")
        send(host, inherited, WINDOWS, 403, "Linux")
        rejected = send(host, windows, UBUNTU, 403, "Windows")
        send(host, windows, WINDOWS, 200, "Windows")
        set_global(host, "macos")
        second_id = send(host, inherited, UBUNTU, 403, "macOS")
        send(host, windows, WINDOWS, 200, "Windows")
        print("Verified outbound UA, global changes, and independent keys", flush=True)

        account = host.admin("/api/admin/accounts?page=1&pageSize=20")["items"][0]
        host.admin("/api/admin/accounts/update", {"accountId": account["id"], "enabled": True,
                   "concurrencyLimit": 16, "weight": 1, "groupIds": [],
                   "connection": {"baseUrl": host.upstream, "transport": "prefer_websocket"}})
        before = len(host.records)
        subprocess.run(["node", str(ROOT / "tests/host-errors.mjs")], check=True, timeout=35,
                       env={**os.environ, "CPR_TEST_URL": host.url, "CPR_TEST_WINDOWS_KEY": windows["plaintextKey"]})
        assert len(host.records) == before, "error-delivery checks reached upstream"
        for _ in range(25):
            send(host, windows, UBUNTU, 403, "Windows")
        verify_page(host, instance, rejected, windows)
        latency = verify_storage(host, instance, rejected, windows, second_id)
        print("Verified SQLite records, rejection under write lock, and seven-day retention", flush=True)
    return {"packageSha256": digest, "lockedRejectionMs": latency, "checks": [
        "package-install", "outbound-platform", "global-inheritance", "key-override", "zero-upstream-on-denial",
        "http-error", "streaming-http-error", "websocket-error", "sqlite-record", "write-lock", "retention",
        "private-management-route", "management-page-browser",
    ]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", type=Path, required=True, help="extracted codex-proxy-rs release directory")
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    args = parser.parse_args()
    hosts = json.loads((ROOT / "host-compatibility.json").read_text())["hosts"]
    metadata = next(h for h in hosts if h["version"] == args.version)
    binary_version = subprocess.check_output([str(args.release.resolve() / "codex-proxy-rs"), "--version"], text=True).strip()
    assert binary_version == metadata["version"], (binary_version, metadata["version"])
    result = {"hostVersion": binary_version, "hostRevision": metadata["revision"], **verify(args.release, args.package)}
    args.evidence.parent.mkdir(parents=True, exist_ok=True)
    args.evidence.write_text(json.dumps(result, indent=2) + "\n")
