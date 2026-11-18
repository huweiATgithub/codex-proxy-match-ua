// Run against a disposable supported host with this plugin enabled and a WS-capable test account.
import assert from "node:assert/strict";
import { once } from "node:events";

const base = process.env.CPR_TEST_URL;
const key = process.env.CPR_TEST_WINDOWS_KEY;
if (!base || !key) throw new Error("Set CPR_TEST_URL and CPR_TEST_WINDOWS_KEY to a test host and a key with a Windows UA.");

const url = new URL("/v1/responses", base);
const headers = {
  Authorization: `Bearer ${key}`,
  "User-Agent": "codex-tui/0.167.2 (Ubuntu 24.4.0; aarch64) WindowsTerminal (codex-tui; 0.167.2)",
};
const request = { model: "gpt-5.4", store: false, input: [{ role: "user", content: "UA compatibility test" }] };
const expected = "UA incompatible. The client User-Agent reports Linux. The effective User-Agent for this key requires Windows.";

function check(error) {
  assert.equal(error.code, "policy_denied");
  assert.equal(error.message, expected);
}

for (const stream of [false, true]) {
  const response = await fetch(url, {
    method: "POST", headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({ ...request, stream }), signal: AbortSignal.timeout(10_000),
  });
  assert.equal(response.status, 403);
  assert.ok(response.headers.get("x-gateway-request-id"));
  check((await response.json()).error);
}

url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
const socket = new WebSocket(url, { headers });
const timeout = AbortSignal.timeout(10_000);
try {
  const rejected = new Promise((resolve, reject) => {
    timeout.addEventListener("abort", () => reject(timeout.reason), { once: true });
    socket.addEventListener("error", () => reject(new Error("WebSocket failed")), { once: true });
    socket.addEventListener("close", () => reject(new Error("Socket closed before the error event")), { once: true });
    socket.addEventListener("message", ({ data }) => {
      try {
        const event = JSON.parse(data);
        if (event.type === "error") resolve(event);
      } catch (error) { reject(error); }
    });
  });
  // Listen before sending, including when the server immediately denies the turn.
  const opened = once(socket, "open", { signal: timeout });
  const [event] = await Promise.all([rejected, opened.then(() => socket.send(JSON.stringify({
    ...request, type: "response.create", stream: true,
  })))]);
  assert.equal(event.status, 403);
  assert.ok(event.request_id);
  assert.equal(event.headers["x-gateway-request-id"], event.request_id);
  check(event.error);
} finally {
  socket.close();
}
console.log("Detailed 403 verified for HTTP JSON, streaming HTTP, and native Responses WebSocket.");
