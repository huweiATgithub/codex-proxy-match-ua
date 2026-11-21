"""Exercise the packaged plugin on one released host and write release evidence."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import time
from urllib.parse import urlencode

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


def send(host, key, ua, expected, family, *, turn_header=None, turn_body=None, input_items=None):
    before = len(host.records)
    request = {
        "model": "gpt-5.4", "stream": False, "store": False, "input": [{"role": "user", "content": "test"}],
    }
    request_headers = {"Authorization": "Bearer " + key["plaintextKey"], "User-Agent": ua}
    if turn_header is not None:
        request_headers["x-codex-turn-metadata"] = json.dumps(turn_header)
    if turn_body is not None:
        request["client_metadata"] = {"x-codex-turn-metadata": json.dumps(turn_body)}
    if input_items is not None:
        request["input"] = input_items
    status, headers, body = host.request("/v1/responses", request, request_headers)
    assert status == expected, (status, body)
    if status == 200:
        assert body["id"].startswith("resp_test_")
        assert len(host.records) == before + 1 and f"({family} " in host.records[-1], host.records[before:]
    else:
        assert len(host.records) == before, "rejected request reached upstream"
        assert body["error"]["code"] == "policy_denied", body
        assert body["error"]["message"] == "Request blocked by policy.", body
    return headers["x-gateway-request-id"]


def verify_storage(host, instance, request_id, key, second_id, sandbox_id, path_id):
    path = host.directory / "runtime" / "huwei.match-ua" / instance / "rejections.sqlite3"

    def recorded():
        assert path.is_file(), f"waiting for {path.name}"
        with sqlite3.connect(path) as db:
            row = db.execute("SELECT client_key_id, client_ua, client_platform, effective_platform, reason "
                             "FROM rejected_requests WHERE request_id=?", (request_id,)).fetchone()
            assert row == (key["id"], UBUNTU, "Linux", "Windows", "platform_mismatch"), row
            sandbox = db.execute("SELECT reason, evidence_field, evidence_pattern, evidence_platform FROM rejected_requests "
                                 "WHERE request_id=?", (sandbox_id,)).fetchone()
            assert sandbox == ("sandbox_platform_mismatch", "body.client_metadata.x-codex-turn-metadata.sandbox",
                               "seccomp", "Linux"), sandbox
            path_row = db.execute("SELECT reason, evidence_field, evidence_pattern, evidence_platform, effective_platform, message "
                                  "FROM rejected_requests WHERE request_id=?", (path_id,)).fetchone()
            assert path_row and path_row[:5] == ("path_os_mismatch", "environment_context.cwd", "/Users/<name>", "macOS", "Linux"), path_row
            assert "private-fixture" not in path_row[5], path_row
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


def verify_name_refresh(host, instance, request_id, account):
    view = next(view for view in host.admin("/api/admin/plugins/extensions") if view["target"]["instanceId"] == instance)
    target = view["target"]
    route = f"/api/admin/plugins/extensions/{instance}/{target['artifactSha256']}/{target['revision']}/api/api/rejections"

    def entry(request_id):
        items = host.admin(route + "?" + urlencode({"requestId": request_id}))["items"]
        assert len(items) == 1, "waiting for the diagnostic writer"
        return items[0]

    def initial():
        row = entry(request_id)
        assert (row["clientKeyName"], row["accountName"]) == ("windows", account["name"]), "waiting for the initial name directory"
    wait(initial, timeout=100)
    archived = create_key(host, "Retired Windows key", preset("windows"))
    archived_id = send(host, archived, UBUNTU, 403, "Windows")

    def resolved():
        row = entry(archived_id)
        assert row["clientKeyName"] == "Retired Windows key", "waiting for maintenance to resolve the new key"
    wait(resolved, timeout=100)
    host.admin("/api/admin/client-keys/delete", {"id": archived["id"]})
    assert entry(archived_id)["clientKeyName"] == "Retired Windows key"
    return archived_id


def verify_page(host, instance, request_id, key, account, archived_id, path_id):
    subprocess.run(["node", str(ROOT / "tests/management-page.mjs")], check=True, timeout=60, env={
        **os.environ, "CPR_TEST_URL": host.url, "CPR_TEST_COOKIE": host.cookie, "CPR_TEST_INSTANCE": instance,
        "CPR_TEST_REQUEST_ID": request_id, "CPR_TEST_KEY_ID": key["id"], "CPR_TEST_KEY_NAME": "windows",
        "CPR_TEST_ACCOUNT_ID": account["id"], "CPR_TEST_ACCOUNT_NAME": account["name"],
        "CPR_TEST_ARCHIVED_ID": archived_id, "CPR_TEST_CLIENT_UA": UBUNTU, "CPR_TEST_PATH_ID": path_id,
    })


def verify(release, archive):
    payload = archive.read_bytes()
    digest = hashlib.sha256(payload).hexdigest()
    with run_host(release) as host:
        host.admin("/api/admin/accounts/import", {"provider": "openai", "data": {
            "authentication_kind": "api_key", "name": "loopback", "base_url": host.upstream,
            "api_key": "sk-loopback-fixture", "transport": "http"}})
        set_global(host, "linux")
        inherited = create_key(host, "inherited")
        windows = create_key(host, "windows", preset("windows"))
        upload = host.admin("/api/admin/plugins/artifacts/upload?sha256=" + digest, payload)
        instance = upload["defaultInstanceId"]

        def enabled():
            assert any(row["id"] == instance and row["enabled"] for row in host.admin("/api/admin/plugins/instances"))
        wait(enabled)
        print("Installed package", digest, flush=True)
        send(host, inherited, UBUNTU, 200, "Linux")
        send(host, inherited, UBUNTU, 200, "Linux", turn_body={"sandbox": "seccomp", "workspaces": {"/mnt/d/Users/name": {}}})
        context = {"role": "user", "content": [{"type": "input_text",
                   "text": "<environment_context><cwd>/Users/private-fixture/repo</cwd></environment_context>"}]}
        prompt = {"role": "user", "content": "hello"}
        path_id = send(host, inherited, UBUNTU, 403, "Linux", input_items=[context, prompt])
        send(host, inherited, UBUNTU, 200, "Linux", input_items=[
            context, prompt, {"role": "assistant", "content": [{"type": "output_text", "text": "done"}]}, prompt])
        send(host, inherited, WINDOWS, 403, "Linux")
        rejected = send(host, windows, UBUNTU, 403, "Windows")
        send(host, windows, WINDOWS, 200, "Windows")
        set_global(host, "macos")
        second_id = send(host, inherited, UBUNTU, 403, "macOS")
        send(host, windows, WINDOWS, 200, "Windows")
        print("Verified outbound UA, global changes, and independent keys", flush=True)
        send(host, windows, WINDOWS, 200, "Windows", turn_body={"sandbox": "windows_elevated"})
        send(host, windows, WINDOWS, 200, "Windows", turn_body={"sandbox": "external"})
        send(host, windows, WINDOWS, 200, "Windows", turn_body={"workspaces": {"//wsl.localhost/Ubuntu/home/name": {}}})
        send(host, windows, WINDOWS, 403, "Windows", turn_header={"sandbox": "seatbelt"})
        sandbox_id = send(host, windows, WINDOWS, 403, "Windows", turn_header={"sandbox": "windows_sandbox"},
                          turn_body={"sandbox": "seccomp"})
        print("Verified sandbox and workspace compatibility before upstream delivery", flush=True)

        account = host.admin("/api/admin/accounts?page=1&pageSize=20")["items"][0]
        assert account["name"] and account["name"] != account["id"]
        host.admin("/api/admin/accounts/update", {"accountId": account["id"], "enabled": True,
                   "concurrencyLimit": 16, "weight": 1, "groupIds": [],
                   "connection": {"baseUrl": host.upstream, "transport": "prefer_websocket"}})
        before = len(host.records)
        subprocess.run(["node", str(ROOT / "tests/host-errors.mjs")], check=True, timeout=35,
                       env={**os.environ, "CPR_TEST_URL": host.url, "CPR_TEST_WINDOWS_KEY": windows["plaintextKey"]})
        assert len(host.records) == before, "error-delivery checks reached upstream"
        for _ in range(25):
            send(host, windows, UBUNTU, 403, "Windows")
        archived_id = verify_name_refresh(host, instance, rejected, account)
        verify_page(host, instance, rejected, windows, account, archived_id, path_id)
        latency = verify_storage(host, instance, rejected, windows, second_id, sandbox_id, path_id)
        print("Verified SQLite records, rejection under write lock, and seven-day retention", flush=True)
    return {"packageSha256": digest, "lockedRejectionMs": latency, "checks": [
        "package-install", "outbound-platform", "global-inheritance", "key-override", "zero-upstream-on-denial",
        "sandbox-platform", "workspace-path-platform", "stateless-current-input", "private-os-evidence", "http-error", "streaming-http-error",
        "websocket-error", "sqlite-record", "write-lock", "retention",
        "private-management-route", "management-page-browser", "rejection-table",
        "cached-key-and-account-names", "background-name-refresh", "deleted-key-name",
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
