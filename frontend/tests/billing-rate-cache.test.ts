import { afterEach, describe, expect, test } from "bun:test";
import { cache, SWRGlobalState } from "swr/_internal";
import type { BillingRateRecord } from "../src/lib/api";
import {
  SWR_KEYS,
  billingRatesForProfileSWRKey,
  copyPricingProfile,
  deletePricingProfileOptimistic,
  renamePricingProfileModel,
  upsertBillingRateOptimistic,
} from "../src/lib/swr";

const originalFetch = globalThis.fetch;
const keys = new Set<string>();
const revalidators = SWRGlobalState.get(cache)![0];

function seed(key: string, data: unknown) {
  keys.add(key);
  cache.set(key, { _k: key, data });
}

function rows(key: string): BillingRateRecord[] | undefined {
  return cache.get(key)?.data;
}

function rate(id: string, profile: string): BillingRateRecord {
  return {
    id, pricing_profile: profile, source: "catalog", model_pattern: "model-a",
    rate_kind: "token", usage_class: "input_uncached", unit: "token",
    unit_price_nano: "1000", unit_price_currency: "USD", peak_unit_price_nano: "2000",
    match_json: {}, priority: 5, enabled: true, raw_json: {}, updated_at: "2026-01-01T00:00:00Z",
  };
}

afterEach(() => {
  globalThis.fetch = originalFetch;
  for (const key of keys) {
    cache.delete(key);
    delete revalidators[key];
  }
  keys.clear();
});

describe("billing-rate cache isolation", () => {
  test("a new rate updates only its profile and the complete catalog", async () => {
    const alpha = rate("alpha", "alpha");
    const beta = rate("beta", "beta");
    const added = rate("new", "alpha");
    const alphaKey = billingRatesForProfileSWRKey("alpha");
    const betaKey = billingRatesForProfileSWRKey("beta");
    seed(SWR_KEYS.BILLING_RATES, [alpha, beta]);
    seed(alphaKey, [alpha]);
    seed(betaKey, [beta]);
    globalThis.fetch = (async () => {
      expect(rows(alphaKey)?.map((row) => row.id)).toEqual(["alpha", "new"]);
      expect(rows(betaKey)).toEqual([beta]);
      expect(rows(SWR_KEYS.BILLING_RATES)?.map((row) => row.id)).toEqual(["alpha", "beta", "new"]);
      return Response.json(added);
    }) as typeof fetch;

    await upsertBillingRateOptimistic(added.id, added, [alpha]);
    expect(rows(betaKey)).toEqual([beta]);
    expect(rows(alphaKey)?.[1]).toEqual(added);
  });

  test("moving a row removes it from its former profile and preserves omitted fields", async () => {
    const existing = rate("moved", "alpha");
    const alphaKey = billingRatesForProfileSWRKey("alpha");
    const betaKey = billingRatesForProfileSWRKey("beta");
    const serverRow = { ...existing, pricing_profile: "beta", source: "manual" };
    seed(alphaKey, [existing]);
    seed(betaKey, []);
    seed(SWR_KEYS.BILLING_RATES, [existing]);
    globalThis.fetch = (async () => {
      expect(rows(alphaKey)).toEqual([]);
      const retained = {
        pricing_profile: "beta", source: "manual", unit_price_currency: "USD",
        unit_price_nano: "1000", peak_unit_price_nano: "2000", model_pattern: "model-a",
        priority: 5,
      };
      expect(rows(betaKey)?.[0]).toMatchObject(retained);
      expect(rows(SWR_KEYS.BILLING_RATES)?.[0]).toMatchObject(retained);
      return Response.json(serverRow);
    }) as typeof fetch;

    await upsertBillingRateOptimistic(existing.id, { pricing_profile: "beta", unit_price_currency: undefined }, [existing]);
    expect(rows(betaKey)).toEqual([serverRow]);
  });

  test("a scoped write leaves an unloaded complete catalog unloaded", async () => {
    const added = rate("new", "alpha");
    seed(SWR_KEYS.BILLING_RATES, undefined);
    seed(billingRatesForProfileSWRKey("alpha"), []);
    globalThis.fetch = (async () => Response.json(added)) as typeof fetch;

    await upsertBillingRateOptimistic(added.id, added, []);
    expect(rows(SWR_KEYS.BILLING_RATES)).toBeUndefined();
  });

  test("a rejected profile move restores cached rows without a successful refetch", async () => {
    const existing = rate("moved", "alpha");
    const beta = rate("beta", "beta");
    const alphaKey = billingRatesForProfileSWRKey("alpha");
    const betaKey = billingRatesForProfileSWRKey("beta");
    seed(SWR_KEYS.BILLING_RATES, [existing, beta]);
    seed(alphaKey, [existing]);
    seed(betaKey, [beta]);
    globalThis.fetch = (async () => Response.json({ error: "offline" }, { status: 503 })) as typeof fetch;

    await expect(upsertBillingRateOptimistic(existing.id, { pricing_profile: "beta" }, [existing])).rejects.toThrow();
    expect(rows(SWR_KEYS.BILLING_RATES)).toEqual([existing, beta]);
    expect(rows(alphaKey)).toEqual([existing]);
    expect(rows(betaKey)).toEqual([beta]);
  });

  test("rollback preserves a later successful write to the same row", async () => {
    const existing = rate("same", "alpha");
    const alphaKey = billingRatesForProfileSWRKey("alpha");
    seed(SWR_KEYS.BILLING_RATES, [existing]);
    seed(alphaKey, [existing]);
    const pending = Promise.withResolvers<Response>();
    const started = Promise.withResolvers<void>();
    let calls = 0;
    const saved = { ...existing, source: "manual", unit_price_nano: "3000" };
    globalThis.fetch = (async () => {
      if (++calls === 1) {
        started.resolve();
        return pending.promise;
      }
      return Response.json(saved);
    }) as typeof fetch;

    const first = upsertBillingRateOptimistic(existing.id, { unit_price_nano: "2000" }, [existing]);
    const firstRejected = first.catch((error: unknown) => error);
    await started.promise;
    await upsertBillingRateOptimistic(existing.id, { unit_price_nano: "3000" }, [existing]);
    pending.resolve(Response.json({ error: "conflict" }, { status: 409 }));
    expect(await firstRejected).toBeInstanceOf(Error);
    expect(rows(SWR_KEYS.BILLING_RATES)).toEqual([saved]);
    expect(rows(alphaKey)).toEqual([saved]);
  });
});

describe("pricing-profile mutation revalidation", () => {
  for (const operation of ["copy", "delete", "rename"] as const) {
    test(`${operation} refreshes every populated rate view and Provider list`, async () => {
      const refreshed: string[] = [];
      const expected = [
        SWR_KEYS.BILLING_RATES,
        SWR_KEYS.BILLING_RATE_PROFILES,
        SWR_KEYS.PROVIDERS,
        billingRatesForProfileSWRKey("alpha"),
        billingRatesForProfileSWRKey("beta"),
      ];
      for (const key of expected) {
        seed(key, []);
        revalidators[key] = [async () => { refreshed.push(key); }];
      }
      globalThis.fetch = (async () => Response.json({
        target_profile: "beta", copied: 1, deleted_rates: 1,
        target_model: "model-b", written: 1, removed: 1, synchronized_retained: 0,
      })) as typeof fetch;

      if (operation === "copy") await copyPricingProfile("alpha", "beta");
      if (operation === "delete") await deletePricingProfileOptimistic("alpha", []);
      if (operation === "rename") await renamePricingProfileModel("alpha", "model-a", "model-b");

      expect(refreshed.sort()).toEqual(expected.sort());
    });
  }
});
