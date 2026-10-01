import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium, expect } from "@playwright/test";

const build = await Bun.build({
  entrypoints: [fileURLToPath(new URL("./fixtures/users-load-errors.tsx", import.meta.url))],
  target: "browser",
  define: { "process.env.NODE_ENV": '"production"' },
});
if (!build.success) throw new AggregateError(build.logs, "Browser fixture build failed");
const bundle = await build.outputs[0].text();
const server = Bun.serve({
  hostname: "127.0.0.1",
  port: 0,
  fetch(request) {
    return new URL(request.url).pathname === "/fixture.js"
      ? new Response(bundle, { headers: { "Content-Type": "text/javascript" } })
      : new Response('<div id="root"></div><script type="module" src="/fixture.js"></script>', {
        headers: { "Content-Type": "text/html" },
      });
  },
});
const browser = await chromium.launch({
  executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH,
});
const dependencies = ["/users", "/groups", "/billing-plans"];
const providerDependencies = ["/providers", "/groups", "/settings", "/transforms/registry", "/model-metadata"];
const cases = [
  ...dependencies.map((failed) => ({ view: "users", failed, reads: dependencies, add: "users.addUser", label: "Fixture User" })),
  { view: "groups", failed: "/groups", reads: ["/groups"], add: "groups.create", label: "Fixture Group" },
  ...providerDependencies.map((failed) => ({ view: "providers", failed, reads: providerDependencies, add: "providers.addProvider", label: "Fixture Provider" })),
];
const user = {
  id: "fixture-user", username: "Fixture User", role: "super_admin",
  account_class: "standard", enabled: true, balance_nano_usd: "0",
  balance_unlimited: false, created_at: "2026-01-01T00:00:00Z", groups: [],
};

try {
  for (const { view, failed, reads, add, label } of cases) {
    const page = await browser.newPage();
    let failing = true;
    const requests: string[] = [];
    const mutations: string[] = [];
    const errors: string[] = [];
    const unexpected: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.route("**/*", async (route) => {
      const req = route.request();
      const url = new URL(req.url());
      if (url.origin !== server.url.origin) return route.abort();
      if (!url.pathname.startsWith("/api/")) return route.continue();
      if (req.method() !== "GET") {
        mutations.push(`${req.method()} ${url.pathname}`);
        return route.abort();
      }
      const path = url.pathname.replace("/api/dashboard", "");
      requests.push(path);
      if (path === failed && failing) return route.fulfill({ status: 500, json: { error: "Fixture read failure" } });
      const responses: Record<string, unknown> = {
        "/auth/me": user, "/me": user, "/users": [user],
        "/groups": { groups: [{ id: "fixture-group", name: "Fixture Group", account_class: "standard", description: "", sort_order: 0, user_selectable: true }] },
        "/billing-plans": [], "/settings": {}, "/transforms/registry": [], "/model-metadata": [],
        "/billing-rates/profiles": [],
        "/providers": [{ id: "fixture-provider", name: "Fixture Provider", group_id: "fixture-group",
          enabled: true, priority: 0, channel: { id: "fixture-channel", models: {}, provider_type: "openai" } }],
        "/store/exchange-rate": { cny_per_usd: "7" },
      };
      if (!(path in responses)) {
        unexpected.push(path);
        return route.fulfill({ status: 500, json: { error: "Unexpected fixture API" } });
      }
      return route.fulfill({ json: responses[path] });
    });
    try {
      await page.goto(`${server.url.href}?view=${view}`);
      await expect(page.getByRole("alert")).toBeVisible();
      await expect(page.getByRole("button", { name: add, exact: true })).toHaveCount(0);
      const before = reads.map((path) => requests.filter((r) => r === path).length);
      failing = false;
      await page.getByRole("button", { name: "common.retry", exact: true }).click();
      await expect(page.getByRole("alert")).toHaveCount(0);
      await expect(page.getByRole("button", { name: add, exact: true })).toBeVisible();
      await expect(page.getByText(label, { exact: true }).first()).toBeVisible();
      reads.forEach((path, index) => assert.ok(requests.filter((r) => r === path).length > before[index]));
      assert.deepEqual(mutations, []);
      assert.deepEqual(errors, []);
      assert.deepEqual(unexpected, []);
      console.log(`PASS ${view} ${failed}: error, manual retry, populated result, no writes`);
    } finally {
      await page.close();
    }
  }
} finally {
  await browser.close();
  server.stop(true);
}
