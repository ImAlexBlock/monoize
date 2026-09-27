import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import en from "../src/locales/en.json";
import zh from "../src/locales/zh.json";
import zhTW from "../src/locales/zh-TW.json";
import ja from "../src/locales/ja.json";
import type { RequestLog } from "../src/lib/api";
import {
  LogRowCells,
  RequestLogChannelTooltipDetails,
  RequestLogModelTooltipDetails,
} from "../src/pages/request-logs/log-row-cells";
import { StoreCurrencyProvider } from "../src/hooks/use-store-currency";

function requestLog(): RequestLog {
  return {
    id: "log-layout",
    created_at: "2026-08-28T00:00:00.000Z",
    status: "success",
    is_stream: false,
    model: "MODEL_VISIBLE",
    provider: { id: "provider-id", name: "PROVIDER_ADMIN_ONLY" },
    channel: { id: "channel-id", name: "CHANNEL_ADMIN_ONLY" },
    user: { id: "user-id", username: "user" },
    api_key: { id: "key-id", name: "key" },
    tokens: {},
    timing: {},
    billing: {},
    error: {},
    affinity: {},
  };
}

function renderRow(isAdmin: boolean, log = requestLog()): string {
  return renderToStaticMarkup(
    <StoreCurrencyProvider>
      <table>
        <tbody>
          <tr>
            <LogRowCells
              affinityTargetNames={new Map()}
              log={log}
              isAdmin={isAdmin}
              showIp={false}
              t={(key) => key}
              onOpenCapture={() => {}}
              onTooltipOpenChange={() => {}}
            />
          </tr>
        </tbody>
      </table>
    </StoreCurrencyProvider>,
  );
}

function renderModelTooltip(
  isAdmin: boolean,
  log = requestLog(),
  t: (key: string) => string = (key) => key,
): string {
  return renderToStaticMarkup(
    <StoreCurrencyProvider>
      <RequestLogModelTooltipDetails
        log={log}
        isAdmin={isAdmin}
        t={t}
      />
    </StoreCurrencyProvider>,
  );
}

function renderChannelTooltip(
  isAdmin: boolean,
  log = requestLog(),
  t: (key: string) => string = (key) => key,
): string {
  return renderToStaticMarkup(
    <RequestLogChannelTooltipDetails
      affinityTargetNames={new Map()}
      log={log}
      isAdmin={isAdmin}
      t={t}
    />,
  );
}

describe("request log model cell layout (FL9)", () => {
  test("does not render Provider identifiers in the user Model tooltip", () => {
    const html = renderModelTooltip(false);

    expect(html).toContain("MODEL_VISIBLE");
    expect(html).not.toContain("provider-id");
    expect(renderModelTooltip(true)).toContain("provider-id");
  });

  test("renders one centered model line without Channel data for a user", () => {
    const html = renderRow(false);

    expect(html).toContain("MODEL_VISIBLE");
    expect(html).toContain("h-9");
    expect(html).toContain("justify-center");
    expect(html).not.toContain("CHANNEL_ADMIN_ONLY");
    expect(html).not.toContain("PROVIDER_ADMIN_ONLY");
    expect(html).not.toContain("channel-id");
    expect(html).not.toContain("provider-id");
  });

  test("retains the visible Channel line for an admin", () => {
    const html = renderRow(true);

    expect(html).toContain("MODEL_VISIBLE");
    expect(html).toContain("CHANNEL_ADMIN_ONLY");
  });
});

describe("actual upstream response model visibility (FL9c)", () => {
  const actualModel = "UPSTREAM_RESPONSE_MODEL_ADMIN_ONLY";

  for (const [locale, translation, label] of [
    ["en", en, "Actual Model"],
    ["zh", zh, "实际模型"],
    ["zh-TW", zhTW, "實際模型"],
    ["ja", ja, "実際のモデル"],
  ] as const) {
    test(`renders the actual model with the ${locale} label in the admin Model tooltip`, () => {
      const i18n = createInstance();
      void i18n.init({
        lng: locale,
        resources: { [locale]: { translation } },
        initImmediate: false,
      });
      const html = renderModelTooltip(
        true,
        { ...requestLog(), upstream_response_model: actualModel },
        (key) => i18n.t(key),
      );

      expect(html).toContain(label);
      expect(html).toContain(actualModel);
    });

    test(`renders the actual model with the ${locale} label in the admin Channel tooltip`, () => {
      const i18n = createInstance();
      void i18n.init({
        lng: locale,
        resources: { [locale]: { translation } },
        initImmediate: false,
      });
      const html = renderChannelTooltip(
        true,
        { ...requestLog(), upstream_response_model: actualModel },
        (key) => i18n.t(key),
      );

      expect(html).toContain(label);
      expect(html).toContain(actualModel);
    });
  }

  test("does not expose the actual model in a user row or Model tooltip", () => {
    const log = { ...requestLog(), upstream_response_model: actualModel };

    expect(renderRow(false, log)).not.toContain(actualModel);
    expect(renderModelTooltip(false, log)).not.toContain(actualModel);
    expect(renderModelTooltip(false, log)).not.toContain("requestLogs.upstreamResponseModel");
    expect(renderChannelTooltip(false, log)).toBe("");
  });

  test("renders the actual model once beside the ModelBadge for admins", () => {
    const html = renderRow(true, {
      ...requestLog(),
      upstream_response_model: actualModel,
    });

    expect(html.match(new RegExp(actualModel, "g"))).toHaveLength(1);
    expect(html).toContain(`↳ ${actualModel}`);
    expect(html).toContain("CHANNEL_ADMIN_ONLY");
  });

  for (const upstreamResponseModel of [undefined, null, ""]) {
    test(`omits the actual-model label when the value is ${String(upstreamResponseModel)}`, () => {
      const log = { ...requestLog(), upstream_response_model: upstreamResponseModel };

      expect(renderRow(true, log)).not.toContain("↳");
      expect(renderModelTooltip(true, log)).not.toContain("requestLogs.upstreamResponseModel");
      expect(renderChannelTooltip(true, log)).not.toContain("requestLogs.upstreamResponseModel");
    });
  }
});
