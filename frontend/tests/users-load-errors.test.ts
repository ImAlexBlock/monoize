import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";

const page = readFileSync(new URL("../src/pages/users.tsx", import.meta.url), "utf8");

test("user management prioritizes dependency errors over loading and editing", () => {
  expect(page).toContain("usersError || groupsError || plansError");
  expect(page).toContain('role="alert"');
  expect(page.indexOf("if (loadError)")).toBeLessThan(page.indexOf("if (isLoading || groupsLoading || plansLoading)"));
  expect(page.indexOf("if (isLoading || groupsLoading || plansLoading)")).toBeLessThan(page.indexOf("<Dialog open={createOpen}"));
});

test("user management retry only revalidates its three dependencies", () => {
  const guard = page.slice(page.indexOf("if (loadError)"), page.indexOf("if (isLoading || groupsLoading || plansLoading)"));
  expect(guard).toContain("void reloadUsers()");
  expect(guard).toContain("void reloadGroups()");
  expect(guard).toContain("void reloadPlans()");
  expect(guard).not.toContain("Optimistic(");
  expect(guard).not.toContain("api.");
});
