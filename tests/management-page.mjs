import assert from "node:assert/strict";
import { chromium } from "playwright";

const { CPR_TEST_URL: base, CPR_TEST_COOKIE: cookie, CPR_TEST_INSTANCE: instance,
  CPR_TEST_REQUEST_ID: requestId, CPR_TEST_KEY_ID: keyId, CPR_TEST_CLIENT_UA: clientUa } = process.env;
const index = await fetch(new URL("/api/admin/plugins/extensions", base), { headers: { Cookie: cookie } });
assert.equal(index.status, 200);
const view = (await index.json()).data.find(item => item.target.instanceId === instance);
const entry = view.pages.find(page => page.id === "rejections");
assert.equal(entry.title, "UA rejections");
assert.equal(view.resources.find(resource => resource.path === entry.entry).public, false);
assert.deepEqual(view.callbacks, []);
const prefix = "/api/admin/plugins/extensions/" + instance + "/" + view.target.artifactSha256 + "/" + view.target.revision;
const route = new URL(prefix + "/api/api/rejections", base);
for (const url of [route, new URL(prefix + "/resources/" + entry.entry, base)]) {
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
  const summaries = ui.locator("summary");
  const next = ui.getByRole("button", { name: "Next", exact: true });
  await next.waitFor();
  const firstPage = await summaries.locator("code").allTextContents();
  assert.equal(firstPage.length, 25);
  await next.click();
  await ui.getByRole("status").filter({ hasText: "Page 2" }).waitFor();
  const secondPage = await summaries.locator("code").allTextContents();
  assert.ok(secondPage.length && secondPage.every(id => !firstPage.includes(id)));
  await ui.getByRole("button", { name: "Previous" }).click();
  await ui.getByRole("status").filter({ hasText: "Page 1" }).waitFor();
  assert.deepEqual(await summaries.locator("code").allTextContents(), firstPage);
  const search = ui.getByLabel("Request ID", { exact: true });
  await search.fill(requestId);
  await search.press("Enter");
  await ui.getByRole("status").filter({ hasText: "1 rejection · Page 1" }).waitFor();
  const row = ui.locator("details").filter({ hasText: requestId });
  await row.locator("summary").click();
  await row.getByText("UA incompatible. The client User-Agent reports Linux. The effective User-Agent for this key requires Windows.", { exact: true }).waitFor();
  await row.getByText(clientUa, { exact: true }).waitFor();
  assert.match(await row.locator("dt", { hasText: "Effective key User-Agent" }).locator("+ dd").innerText(), /\(Windows /);
  await row.getByText(keyId, { exact: true }).waitFor();
  if (process.env.CPR_TEST_SCREENSHOT) await page.screenshot({ path: process.env.CPR_TEST_SCREENSHOT });
  await page.setViewportSize({ width: 390, height: 800 });
  assert.ok(await ui.locator("html").evaluate(node => node.scrollWidth <= node.clientWidth + 1), "mobile page fits the iframe");
  await search.fill("missing-" + requestId);
  await search.press("Enter");
  await ui.getByText("No rejections match this request ID.", { exact: true }).waitFor();
  await ui.getByRole("button", { name: "Refresh" }).click();
  await ui.getByText("No rejections match this request ID.", { exact: true }).waitFor();
  assert.deepEqual(errors, []);
  console.log("Admin browser verified private access, pagination, search, failure details and the mobile layout.");
} finally {
  await browser.close();
}
