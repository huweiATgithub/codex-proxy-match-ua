import assert from "node:assert/strict";
import { chromium } from "playwright";

const { CPR_TEST_URL: base, CPR_TEST_COOKIE: cookie, CPR_TEST_INSTANCE: instance,
  CPR_TEST_REQUEST_ID: requestId, CPR_TEST_KEY_ID: keyId, CPR_TEST_KEY_NAME: keyName,
  CPR_TEST_ACCOUNT_ID: accountId, CPR_TEST_ACCOUNT_NAME: accountName, CPR_TEST_ARCHIVED_ID: archivedId,
  CPR_TEST_CLIENT_UA: clientUa } = process.env;
const index = await fetch(new URL("/api/admin/plugins/extensions", base), { headers: { Cookie: cookie } });
assert.equal(index.status, 200);
const view = (await index.json()).data.find(item => item.target.instanceId === instance);
const entry = view.pages.find(page => page.id === "rejections");
assert.equal(entry.title, "UA rejections");
assert.ok(view.resources.every(resource => !resource.public));
assert.deepEqual(view.callbacks, []);
const prefix = "/api/admin/plugins/extensions/" + instance + "/" + view.target.artifactSha256 + "/" + view.target.revision;
const route = new URL(prefix + "/api/api/rejections", base);
for (const url of [route, ...view.resources.map(resource => new URL(prefix + "/resources/" + resource.path, base))]) {
  const anonymous = await fetch(url);
  assert.ok([401, 403].includes(anonymous.status), "diagnostic data and page require an admin session");
}
const invalid = await fetch(new URL(route + "?before=broken"), { headers: { Cookie: cookie } });
assert.equal(invalid.status, 400);

const browser = await chromium.launch();
try {
  const context = await browser.newContext({ viewport: { width: 1200, height: 850 } });
  const separator = cookie.indexOf("=");
  await context.addCookies([{ name: cookie.slice(0, separator), value: cookie.slice(separator + 1), url: base }]);
  const page = await context.newPage();
  page.setDefaultTimeout(12_000);
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.goto(new URL("/plugins", base).href);
  await page.getByRole("navigation").getByText(entry.title, { exact: true }).click();
  await page.waitForURL(new URL("/plugins/" + instance + "/" + entry.id, base).href);
  const ui = page.frameLocator("iframe");
  const table = ui.getByRole("table");
  const ids = table.locator("tbody tr > td:last-child");
  const next = ui.getByRole("button", { name: "Next", exact: true });
  await next.waitFor().catch(cause => {
    assert.deepEqual(errors, [], "The packaged page failed to start");
    throw cause;
  });
  assert.deepEqual(await table.getByRole("columnheader").allTextContents(), ["Rejected at", "Client key", "Upstream account", "Client → key OS", "Request ID"]);
  const firstPage = await ids.allTextContents();
  assert.equal(firstPage.length, 25);
  await next.click();
  await ui.getByRole("status").filter({ hasText: "Page 2" }).waitFor();
  const secondPage = await ids.allTextContents();
  assert.ok(secondPage.length && secondPage.every(id => !firstPage.includes(id)));
  await ui.getByRole("button", { name: "Previous" }).click();
  await ui.getByRole("status").filter({ hasText: "Page 1" }).waitFor();
  assert.deepEqual(await ids.allTextContents(), firstPage);
  const search = ui.getByLabel("Request ID", { exact: true });
  await search.fill(requestId);
  await search.press("Enter");
  await ui.getByRole("status").filter({ hasText: "1 rejection · Page 1" }).waitFor();
  const row = table.getByRole("row").filter({ hasText: requestId });
  for (const name of [keyName, accountName, "Linux → Windows"]) await row.getByRole("cell", { name, exact: true }).waitFor();
  const toggle = row.getByRole("button", { name: "Details for " + requestId });
  await toggle.focus();
  await toggle.press("Enter");
  const message = "UA incompatible. The client User-Agent reports Linux. The effective User-Agent for this key requires Windows.";
  const detail = table.getByRole("row").filter({ hasText: message });
  await detail.getByText(clientUa, { exact: true }).waitFor();
  await detail.getByText(requestId, { exact: true }).waitFor();
  assert.match(await detail.locator("dt", { hasText: "Effective key User-Agent" }).locator("+ dd").innerText(), /\(Windows /);
  const displayed = await table.innerText();
  for (const hidden of [keyId, accountId, "Reason code", "platform_mismatch"]) assert.ok(!displayed.includes(hidden), hidden);
  if (process.env.CPR_TEST_SCREENSHOT) await page.screenshot({ path: process.env.CPR_TEST_SCREENSHOT });
  await page.setViewportSize({ width: 390, height: 800 });
  assert.ok(await ui.locator("html").evaluate(node => node.scrollWidth <= node.clientWidth + 1), "mobile page fits the iframe");
  await search.fill(archivedId);
  await search.press("Enter");
  await table.getByRole("row").filter({ hasText: archivedId }).getByRole("cell", { name: "Retired Windows key" }).waitFor();
  await search.fill("missing-" + requestId);
  await search.press("Enter");
  await ui.getByText("No rejections match this request ID.", { exact: true }).waitFor();
  await ui.getByRole("button", { name: "Refresh" }).click();
  await ui.getByText("No rejections match this request ID.", { exact: true }).waitFor();
  assert.deepEqual(errors, []);
  console.log("Admin browser verified the private table, key and account names, deleted key, pagination, search, failure details and mobile layout.");
} finally {
  await browser.close();
}
