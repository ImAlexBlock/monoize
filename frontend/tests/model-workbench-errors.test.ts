import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";

const workbench = readFileSync(new URL("../src/pages/model-metadata/model-workbench.tsx", import.meta.url), "utf8");
const page = readFileSync(new URL("../src/pages/model-metadata.tsx", import.meta.url), "utf8");

test("model workbench prioritizes request errors over empty data", () => {
  expect(workbench).toContain("metadataError || profilesError || ratesError");
  expect(workbench.indexOf("{loadError ? (")).toBeLessThan(workbench.indexOf("rows.length === 0 ? ("));
  expect(workbench).toContain('role="alert"');
  expect(page).toContain("metadataError={metadataError}");
});

test("retry only reloads existing models and pricing", () => {
  const retry = workbench.slice(workbench.indexOf('action={'), workbench.indexOf(') : profilesLoading'));
  expect(retry).toContain("onRetryMetadata()");
  expect(retry).toContain("revalidateProfiles()");
  expect(retry).toContain("revalidateRates()");
  expect(retry).not.toContain("runSync(");
});
