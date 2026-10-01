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
const user = {
  id: "fixture-user", username: "Fixture User", role: "super_admin",
  account_class: "standard", enabled: true, balance_nano_usd: "0",
  balance_unlimited: false, created_at: "2026-01-01T00:00:00Z", groups: [],
};

try {
  for (const failed of dependencies) {
    const page = await browser.newPage();
    let failing = true;
    const requests: string[] = [];
    const mutations: string[] = [];
    const errors: string[] = [];
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
        "/groups": { groups: [] }, "/billing-plans": [],
        "/store/exchange-rate": { cny_per_usd: "7" },
      };
      assert.ok(path in responses, `Unexpected fixture API: ${path}`);
      return route.fulfill({ json: responses[path] });
    });
    try {
      await page.goto(server.url.href);
      await expect(page.getByRole("alert")).toBeVisible();
      await expect(page.getByRole("button", { name: "users.addUser", exact: true })).toHaveCount(0);
      const before = dependencies.map((path) => requests.filter((r) => r === path).length);
      failing = false;
      await page.getByRole("button", { name: "common.retry", exact: true }).click();
      await expect(page.getByRole("alert")).toHaveCount(0);
      await expect(page.getByRole("button", { name: "users.addUser", exact: true })).toBeVisible();
      await expect(page.getByText("Fixture User", { exact: true }).first()).toBeVisible();
      dependencies.forEach((path, index) => assert.ok(requests.filter((r) => r === path).length > before[index]));
      assert.deepEqual(mutations, []);
      assert.deepEqual(errors, []);
      console.log(`PASS ${failed}: error, manual retry, populated result, no writes`);
    } finally {
      await page.close();
    }
  }
} finally {
  await browser.close();
  server.stop(true);
}
