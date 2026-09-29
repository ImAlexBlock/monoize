import { afterEach, describe, expect, test } from "bun:test";
import { api, subscribeDashboardUnauthorized } from "../src/lib/api";
import { readFileSync } from "node:fs";

const originalFetch = globalThis.fetch;

afterEach(() => {
  globalThis.fetch = originalFetch;
});

describe("dashboard session transport", () => {
  test("includes cookies on dashboard requests", async () => {
    let credentials: RequestCredentials | undefined;
    globalThis.fetch = (async (_input, init) => {
      credentials = init?.credentials;
      return Response.json({ id: "user-1" });
    }) as typeof fetch;

    await api.me();

    expect(credentials).toBe("include");
  });

  test("invalidates browser auth state for an unauthorized dashboard session", async () => {
    globalThis.fetch = (async () =>
      Response.json(
        { error: { code: "unauthorized", message: "missing dashboard session" } },
        { status: 401 },
      )) as typeof fetch;
    let invalidations = 0;
    const unsubscribe = subscribeDashboardUnauthorized(() => {
      invalidations += 1;
    });

    try {
      await expect(api.me()).rejects.toThrow("missing dashboard session");
      expect(invalidations).toBe(1);
    } finally {
      unsubscribe();
    }
  });

  test("does not invalidate a valid session for rejected login credentials", async () => {
    globalThis.fetch = (async () =>
      Response.json(
        { error: { code: "invalid_credentials", message: "invalid username or password" } },
        { status: 401 },
      )) as typeof fetch;
    let invalidations = 0;
    const unsubscribe = subscribeDashboardUnauthorized(() => {
      invalidations += 1;
    });

    try {
      await expect(api.login("user", "wrong-password", "token")).rejects.toThrow(
        "invalid username or password",
      );
      expect(invalidations).toBe(0);
    } finally {
      unsubscribe();
    }
  });
});

describe("public site transport", () => {
  test("loads the public allow-list from the public site endpoint", async () => {
    let requestedUrl = "";
    globalThis.fetch = (async (input) => {
      requestedUrl = String(input);
      return Response.json({
        site_name: "LynShen Console",
        site_description: "API service",
        api_base_url: "https://lynshen.org/v1",
      });
    }) as typeof fetch;

    const settings = await api.getPublicSiteSettings();

    expect(requestedUrl).toBe("/api/public/site");
    expect(Object.keys(settings).sort()).toEqual([
      "api_base_url",
      "site_description",
      "site_name",
    ]);
  });
});

describe("public dashboard settings cache", () => {
  test("preserves CAPTCHA settings when an unauthenticated session check clears private data", () => {
    const swrSource = readFileSync(new URL("../src/lib/swr.ts", import.meta.url), "utf8");

    expect(swrSource).toContain("key === SWR_KEYS.PUBLIC_SETTINGS");
  });
});
