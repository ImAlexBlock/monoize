import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium, expect, type Page } from "@playwright/test";

const build = await Bun.build({
  entrypoints: [fileURLToPath(new URL("./fixtures/org-spend-limits.tsx", import.meta.url))],
  target: "browser",
  define: { "process.env.NODE_ENV": '"production"' },
});
if (!build.success) throw new AggregateError(build.logs, "Browser fixture build failed");
const bundle = await build.outputs[0].text();
const server = Bun.serve({
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

const stored = { total_nano_usd: "1000000000", hourly_nano_usd: null, daily_nano_usd: null };
const spent = { total_nano_usd: "0", hourly_nano_usd: "0", daily_nano_usd: "0" };
const limits = {
  space: { limits: stored, spent },
  members: [{ user_id: "member-a", username: "Member A", role: "member", limits: stored, spent }],
  keys: [{ key_id: "key-a", name: "Key A", created_by: "member-a", limits: stored, spent }],
};

async function openPage() {
  const page = await browser.newPage();
  const mutations: unknown[] = [];
  await page.route("**/api/dashboard/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (request.method() === "PUT") {
      mutations.push(request.postDataJSON());
      return route.fulfill({ json: {} });
    }
    if (path.endsWith("/exchange-rate")) return route.fulfill({ json: { cny_per_usd: "7" } });
    if (path.endsWith("/orgs")) return route.fulfill({ json: {
      orgs: [{ id: "test-org", role: "owner" }], creation_limit: 2, creation_used: 1, can_create: true,
    } });
    if (path.endsWith("/limits")) return route.fulfill({ json: limits });
    throw new Error(`Unexpected request: ${request.method()} ${path}`);
  });
  await page.goto(server.url.href);
  await expect(page.locator("#space-spend-total_nano_usd")).toHaveValue("7");
  await page.getByRole("tab", { name: "USD", exact: true }).first().click();
  await expect(page.locator("#space-spend-total_nano_usd")).toHaveValue("1");
  return { page, mutations };
}

async function save(page: Page, target: "space" | "member" | "key") {
  if (target === "key") {
    await page.getByRole("button", { name: "orgLimits.save", exact: true }).click();
  } else {
    await page.getByRole("button", { name: "orgLimits.saveSpaceAndMembers", exact: true }).click();
  }
}

let failed = 0;
async function check(name: string, run: () => Promise<void>) {
  try {
    await run();
    console.log(`PASS ${name}`);
  } catch (error) {
    failed++;
    console.error(`FAIL ${name}`, error);
  }
}

try {
  for (const target of ["space", "member", "key"] as const) {
    const prefix = target === "space" ? "space" : `${target}-${target}-a`;
    await check(`${target} rejects invalid input before sending a mutation`, async () => {
      const { page, mutations } = await openPage();
      try {
        const input = page.locator(`#${prefix}-spend-total_nano_usd`);
        await input.fill("invalid");
        await save(page, target);
        await expect(page.getByText("Spend limit (total) must be a non-negative amount", { exact: true })).toBeVisible();
        assert.deepEqual(mutations, []);
        await expect(input).toHaveValue("invalid");
      } finally { await page.close(); }
    });
    await check(`${target} rejects CNY when the exchange rate becomes invalid`, async () => {
      const { page, mutations } = await openPage();
      try {
        await page.getByRole("tab", { name: "CNY", exact: true }).first().click();
        await page.getByRole("button", { name: "Invalidate exchange rate" }).click();
        await save(page, target);
        await expect(page.getByText("Spend limit (total) must be a non-negative amount", { exact: true })).toBeVisible();
        assert.deepEqual(mutations, []);
      } finally { await page.close(); }
    });
  }
  await check("a currency switch in a different row preserves all edited limits", async () => {
    const { page, mutations } = await openPage();
    try {
      await page.locator("#space-spend-total_nano_usd").fill("2");
      await page.locator("#member-member-a-spend-total_nano_usd").fill("3");
      await page.locator("#key-key-a-spend-total_nano_usd").fill("4");
      await page.getByRole("tab", { name: "CNY", exact: true }).last().click();
      await expect(page.locator("#space-spend-total_nano_usd")).toHaveValue("14");
      await expect(page.locator("#member-member-a-spend-total_nano_usd")).toHaveValue("21");
      await expect(page.locator("#key-key-a-spend-total_nano_usd")).toHaveValue("28");
      await save(page, "space");
      await expect.poll(() => mutations.length).toBe(1);
      assert.deepEqual(mutations[0], {
        space: { total_nano_usd: "2000000000", hourly_nano_usd: null, daily_nano_usd: null },
        members: { "member-a": { total_nano_usd: "3000000000", hourly_nano_usd: null, daily_nano_usd: null } },
      });
      await save(page, "key");
      await expect.poll(() => mutations.length).toBe(2);
      assert.deepEqual(mutations[1], {
        total_nano_usd: "4000000000", hourly_nano_usd: null, daily_nano_usd: null,
      });
    } finally { await page.close(); }
  });
} finally {
  await browser.close();
  server.stop(true);
}
if (failed) throw new Error(`${failed} organization spend-limit browser regressions failed`);
