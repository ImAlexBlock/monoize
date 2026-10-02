import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium, expect } from "@playwright/test";

const build = await Bun.build({
  entrypoints: [fileURLToPath(new URL("./fixtures/playground-composer.tsx", import.meta.url))],
  target: "browser",
  define: { "process.env.NODE_ENV": '"production"' },
});
if (!build.success) throw new AggregateError(build.logs, "Browser fixture build failed");
const bundle = await build.outputs[0].text();
const style = Bun.spawn([
  "bun", "x", "--no-install", "tailwindcss", "-i", "src/index.css", "--minify",
], { stdout: "pipe", stderr: "pipe" });
const css = (await new Response(style.stdout).text()).replace(/@import[^;]+;/g, "");
if (await style.exited) throw new Error(await new Response(style.stderr).text());
const server = Bun.serve({
  port: 0,
  fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === "/fixture.js") return new Response(bundle, { headers: { "Content-Type": "text/javascript" } });
    if (path === "/fixture.css") return new Response(css, { headers: { "Content-Type": "text/css" } });
    return new Response('<link rel="stylesheet" href="/fixture.css"><div id="root"></div><script type="module" src="/fixture.js"></script>', {
      headers: { "Content-Type": "text/html" },
    });
  },
});
const browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH });
let failed = 0;
async function check(name: string, run: () => Promise<void>) {
  try { await run(); console.log(`PASS ${name}`); }
  catch (error) { failed++; console.error(`FAIL ${name}`, error); }
}
try {
  for (const mode of ["chat", "image"] as const) {
    const page = await browser.newPage({ reducedMotion: "reduce" });
    await page.goto(server.url.href);
    const textarea = page.getByRole("textbox", { name: "playground.composerLabel" });
    await expect(textarea).toBeVisible();
    if (mode === "image") await page.getByRole("button", { name: "playground.modeImage", exact: true }).click();
    await check(`${mode}: grows, caps, and shrinks without clipping`, async () => {
      await textarea.fill("A short line");
      const short = (await textarea.boundingBox())!.height;
      await textarea.fill(Array.from({ length: 30 }, () => "Another line").join("\n"));
      await expect.poll(async () => (await textarea.boundingBox())!.height).toBe(200);
      assert.equal(await textarea.evaluate((el) => getComputedStyle(el).overflowY), "auto");
      await textarea.fill("");
      await expect.poll(async () => (await textarea.boundingBox())!.height).toBe(short);
      assert.equal(await textarea.evaluate((el) => getComputedStyle(el).overflowY), "hidden");
    });
    await check(`${mode}: reflows existing text when the composer narrows`, async () => {
      await textarea.fill("A sentence that wraps more lines when the composer becomes narrower. ".repeat(3));
      const wide = (await textarea.boundingBox())!.height;
      await page.locator("#composer-width").evaluate((el) => { el.style.width = "300px"; });
      await expect.poll(async () => (await textarea.boundingBox())!.height).toBeGreaterThan(wide);
      await page.locator("#composer-width").evaluate((el) => { el.style.width = "600px"; });
    });
    await check(`${mode}: preserves one line when Safari underreports scrollHeight`, async () => {
      await textarea.evaluate((el) => Object.defineProperty(el, "scrollHeight", { configurable: true, get: () => 0 }));
      await textarea.fill("x");
      await textarea.evaluate(() => new Promise(requestAnimationFrame));
      const metrics = await textarea.evaluate((el) => {
        const style = getComputedStyle(el);
        return { height: el.getBoundingClientRect().height, minimum: parseFloat(style.lineHeight) + parseFloat(style.paddingTop) + parseFloat(style.paddingBottom) };
      });
      assert.ok(metrics.height >= metrics.minimum, `${metrics.height} clips the required ${metrics.minimum} pixels`);
    });
    await page.close();
  }
} finally { await browser.close(); server.stop(true); }
if (failed) throw new Error(`${failed} composer browser regressions failed`);
