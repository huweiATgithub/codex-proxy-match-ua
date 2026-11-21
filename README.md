# Codex UA platform check

This plugin rejects OpenAI model sends when the original client User-Agent and the User-Agent prepared for the authenticated key declare different OS families. It recognizes Linux, Windows, and macOS. Ubuntu in Windows Terminal is Linux, including when the key uses a Fedora profile or a different architecture. A missing, duplicated, malformed, or unrecognized UA also blocks the send.

If Codex turn metadata declares a platform-specific `sandbox`, it must also agree with the key’s effective OS. `seccomp` identifies Linux, `seatbelt` identifies macOS, and `windows_sandbox`, `windows_elevated`, and `windows_mxc` identify Windows. Missing, neutral (`none`, `external`), and unrecognized backends carry no OS requirement. Permissions such as `sandbox_mode=workspace-write` apply across operating systems.

Each send is checked independently. The plugin inspects Git `workspaces` keys and the current Codex environment’s `cwd` and `filesystem.workspace_roots`. Absolute drive-letter paths and backslash UNC shares indicate Windows, as do the forward-slash spellings of `//wsl.localhost/<distro>` and `//wsl$/<distro>`. Leading `/mnt/<drive-letter>`, `/home/<name>`, and `/root` indicate Linux. Leading `/Users/<name>` indicates macOS. A Linux UA with `/mnt/c/Users/alex` passes the path check. A Linux UA with `/Users/alex` is rejected, even on a genuine Linux mount there.

Missing fields, generic paths such as `/repo`, and shell names carry no OS requirement. For model input, the plugin reads standalone environment fragments in the current user turn. It skips quoted examples, tool results, replayed turns, and separately identified remote environments. Accepted requests keep their original paths.

The minimum host is **codex-proxy-rs v3.33.0**, the first release with plugin RPC 7. The plugin uses middleware v6 for outbound checks and client error delivery, management v2 for the admin page, and maintenance v1 for retention. The SDK rejects a connection with a different RPC version. The immediately preceding release, v3.32.0, speaks RPC 6. [v3.33.0 host contract](https://github.com/zyycn/codex-proxy-rs/blob/v3.33.0/backend/crates/gateway-plugin/runtime/plugin-host-compatibility.json) · [v3.32.0 host contract](https://github.com/zyycn/codex-proxy-rs/blob/v3.32.0/backend/crates/gateway-plugin/runtime/plugin-host-compatibility.json)

Upload the Linux x86-64 package in the proxy’s plugin administration page. Keep the default `outbound`, `request`, and `websocket` bindings enabled. The response bindings give this plugin’s denials consistent wording across host versions. If another outbound plugin edits User-Agent or turn metadata, put this plugin outermost so it checks the prepared request returned by that plugin. The outbound hook compares the original UA with the prepared UA after the host resolves the key’s override or its inherited global profile. It checks the `x-codex-turn-metadata` header and each turn-metadata value in the HTTP body or WebSocket frame, including `client_metadata["x-codex-turn-metadata"]`. A single conflict blocks the send.

A rejected request returns `403 policy_denied` with `Request blocked by policy.` and the gateway request ID. An established Responses WebSocket receives the corresponding generic error event with the turn’s request ID. On a reused upstream WebSocket, the host exposes the candidate UA for that turn.

In the host admin console, open **Plugins → UA rejections**. Search for the gateway request ID and expand the timestamp to see both User-Agents and the private denial reason. For path and sandbox mismatches, the record includes the triggering field, matched path pattern or backend, inferred OS, and expected key OS. Path evidence stores the pattern, such as `/Users/<name>`, without the username or project path. When upgrading, switch the existing instance to the new version to keep its history.

Host maintenance refreshes key and OpenAI account names at startup and on the first maintenance pass at least one minute after the previous refresh. Model requests and admin page views make zero host name lookups. Existing rows show the most recently observed names, including after a rename or deletion. A name the plugin has never observed appears as **Name unavailable**.

Each instance stores diagnostics in `<runtime_data_dir>/huwei.match-ua/<instance_id>/rejections.sqlite3`. From a SQLite shell opened on this file, inspect the latest rejections with

```sql
SELECT datetime(rejected_at, 'unixepoch') AS utc, request_id, client_key_id,
       reason, evidence_field, evidence_pattern, evidence_platform, effective_platform,
       message, client_ua, effective_ua
FROM rejected_requests
ORDER BY rejected_at DESC, id DESC
LIMIT 20;
```

Rows also include the upstream account ID, model, attempt number, upstream transport, and count of UA header values on each side. The first UA header from each side is sampled up to 1,024 bytes. A bounded queue accepts records without waiting for SQLite. A dedicated writer commits up to 32 records per transaction. A full queue, a storage failure, or a process exit can lose diagnostic rows. Host maintenance deletes records older than seven days in indexed batches. SQLite reuses the freed pages.

Build and package on Linux x86-64 with Rust 1.99.1, Node 24, and the v3.52.0 host’s `cpr-plugin` CLI on PATH.

```sh
cargo test --locked
cargo build --release --locked
npm ci --ignore-scripts
npm run build
cpr-plugin package --manifest plugin.json --binary target/release/codex-proxy-match-ua \
  --target x86_64-unknown-linux-gnu --output-dir dist
```

The offline parser tests use the published CLI and Exec samples. [Codex UA v0.167.2](https://github.com/huweiATgithub/codex-ua/releases/tag/v0.167.2)

Pull requests and pushes to `main` build one package and exercise it on released hosts v3.33.0, v3.44.0, and v3.52.0. CI checks key overrides, global inheritance, client-visible errors, SQLite retention, and the authenticated admin page in Chromium. Pushing a matching `v<version>` tag repeats those checks and publishes the tested archive, checksum files, host verification report, and license. [CI workflow](.github/workflows/ci.yml) · [Release workflow](.github/workflows/release.yml)

For the client error regression test, set `CPR_TEST_URL` and `CPR_TEST_WINDOWS_KEY` for a disposable host with a Windows key and a WebSocket-capable OpenAI account. Run `node tests/host-errors.mjs` using Node 24.
