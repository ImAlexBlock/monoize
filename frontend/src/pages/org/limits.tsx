import { useEffect, useRef, useState } from "react";
import { useParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { mutate } from "swr";
import { Gauge } from "lucide-react";
import { api, type OrgLimitsResponse, type OrgSpendLimitSet } from "@/lib/api";
import {
  SPEND_WINDOWS,
  buildSpendLimitPayloadIn,
  nanoToLimitInput,
  rederiveField,
  type SpendLimitDraft,
} from "@/lib/spend-limits";
import { SpendLimitsEditor } from "@/components/SpendLimitsEditor";
import { useStoreExchangeRate } from "@/hooks/use-store-exchange-rate";
import { formatCost } from "../request-logs/utils";
import { useMyOrgs } from "./shared";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

type WindowKey = (typeof SPEND_WINDOWS)[number]["key"];

const WINDOWS = SPEND_WINDOWS;

/** ORGL-20: drafts hold display strings in the editor's current currency;
 * conversion to canonical nano-USD happens once, at save time. */
function draftFromLimits(
  limits: OrgSpendLimitSet,
  currency: "USD" | "CNY",
  cnyPerUsd: string | undefined,
): SpendLimitDraft {
  return {
    total: nanoToLimitInput(limits.total_nano_usd, currency, cnyPerUsd),
    hourly: nanoToLimitInput(limits.hourly_nano_usd, currency, cnyPerUsd),
    daily: nanoToLimitInput(limits.daily_nano_usd, currency, cnyPerUsd),
  };
}

function limitsFromDraft(
  draft: SpendLimitDraft,
  currency: "USD" | "CNY",
  cnyPerUsd: string | undefined,
): OrgSpendLimitSet {
  const payload = buildSpendLimitPayloadIn(draft, currency, cnyPerUsd, true);
  return {
    total_nano_usd: payload.spend_limit_total_nano_usd || null,
    hourly_nano_usd: payload.spend_limit_hourly_nano_usd || null,
    daily_nano_usd: payload.spend_limit_daily_nano_usd || null,
  };
}

/** Adapter: the editor re-derivation wants total/hourly/daily keys. */
function storedNanoForEditor(limits: OrgSpendLimitSet) {
  return {
    total: limits.total_nano_usd,
    hourly: limits.hourly_nano_usd,
    daily: limits.daily_nano_usd,
  };
}

export function OrgLimitsPage() {
  const { orgId } = useParams();
  const { t } = useTranslation();
  const { data: overview } = useMyOrgs();
  const [data, setData] = useState<OrgLimitsResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [limitCurrency, setLimitCurrency] = useState<"USD" | "CNY">("USD");
  const { data: exchangeRate } = useStoreExchangeRate();
  const cnyPerUsd = exchangeRate?.cny_per_usd;
  const rate = cnyPerUsd;
  const [spaceDraft, setSpaceDraft] = useState<SpendLimitDraft>({ total: "", hourly: "", daily: "" });
  const [memberDrafts, setMemberDrafts] = useState<Record<string, SpendLimitDraft>>({});
  const [keyDrafts, setKeyDrafts] = useState<Record<string, SpendLimitDraft>>({});

  const role = overview?.orgs?.find((o) => o.id === orgId)?.role;
  const isOwner = role === "owner";

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    api
      .getOrgLimits(orgId ?? "")
      .then((res) => {
        if (cancelled) return;
        setData(res);
        setSpaceDraft(draftFromLimits(res.space.limits, limitCurrency, rate));
        const md: Record<string, SpendLimitDraft> = {};
        for (const m of res.members) md[m.user_id] = draftFromLimits(m.limits, limitCurrency, rate);
        setMemberDrafts(md);
        const kd: Record<string, SpendLimitDraft> = {};
        for (const k of res.keys) kd[k.key_id] = draftFromLimits(k.limits, limitCurrency, rate);
        setKeyDrafts(kd);
        setError(null);
      })
      .catch((e) => !cancelled && setError(String(e?.message ?? e)))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
    // Re-seed only on org change; currency switches re-derive through the editor.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [orgId]);

  // CNY is the default input currency. Stored limits are nano-USD, so the drafts
  // switch to CNY once both the limits and the fixed rate are loaded; switching
  // earlier would render every stored limit as empty.
  const defaultedToCny = useRef(false);
  useEffect(() => {
    if (defaultedToCny.current || !data || !rate || limitCurrency !== "USD") return;
    defaultedToCny.current = true;
    setLimitCurrency("CNY");
    setSpaceDraft(draftFromLimits(data.space.limits, "CNY", rate));
    const md: Record<string, SpendLimitDraft> = {};
    for (const m of data.members) md[m.user_id] = draftFromLimits(m.limits, "CNY", rate);
    setMemberDrafts(md);
    const kd: Record<string, SpendLimitDraft> = {};
    for (const k of data.keys) kd[k.key_id] = draftFromLimits(k.limits, "CNY", rate);
    setKeyDrafts(kd);
  }, [data, rate, limitCurrency]);

  if (!isOwner) {
    return (
      <div className="p-4 text-sm text-muted-foreground sm:p-6">{t("orgLimits.ownerOnly")}</div>
    );
  }
  if (loading) {
    return (
      <div className="space-y-3 p-4 sm:p-6">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-40 w-full" />
        <Skeleton className="h-40 w-full" />
      </div>
    );
  }
  if (!data) {
    return <div className="p-4 text-sm text-destructive sm:p-6">{error ?? t("orgLimits.loadFailed")}</div>;
  }

  const windowLabel = (key: WindowKey) =>
    t(WINDOWS.find((w) => w.key === key)?.labelKey ?? "");

  const rederiveAll = (nextCurrency: "USD" | "CNY") => {
    const rederiveDraft = (draft: SpendLimitDraft | undefined, stored: OrgSpendLimitSet): SpendLimitDraft => ({
      total: rederiveField(draft?.total ?? "", stored.total_nano_usd, nextCurrency, limitCurrency, rate),
      hourly: rederiveField(draft?.hourly ?? "", stored.hourly_nano_usd, nextCurrency, limitCurrency, rate),
      daily: rederiveField(draft?.daily ?? "", stored.daily_nano_usd, nextCurrency, limitCurrency, rate),
    });
    setLimitCurrency(nextCurrency);
    setSpaceDraft(rederiveDraft(spaceDraft, data.space.limits));
    const md: Record<string, SpendLimitDraft> = {};
    for (const m of data.members) md[m.user_id] = rederiveDraft(memberDrafts[m.user_id], m.limits);
    setMemberDrafts(md);
    const kd: Record<string, SpendLimitDraft> = {};
    for (const k of data.keys) kd[k.key_id] = rederiveDraft(keyDrafts[k.key_id], k.limits);
    setKeyDrafts(kd);
  };

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      // ORGL-10: the owner member row cannot carry member limits; the table
      // already hides it, so the patch must exclude it too. Submitting the
      // owner's (empty) draft made the backend reject the whole save.
      const editableMembers = new Set(
        data.members.filter((m) => m.role !== "owner").map((m) => m.user_id),
      );
      await api.updateOrgLimits(
        orgId ?? "",
        limitsFromDraft(spaceDraft, limitCurrency, rate),
        Object.fromEntries(
          Object.entries(memberDrafts)
            .filter(([id]) => editableMembers.has(id))
            .map(([id, draft]) => [
              id,
              limitsFromDraft(draft, limitCurrency, rate),
            ]),
        ),
      );
      const refreshed = await api.getOrgLimits(orgId ?? "");
      setData(refreshed);
      await mutate(`/api/dashboard/orgs/${orgId}/limits`);
    } catch (e) {
      setError(String((e as Error)?.message ?? e));
    } finally {
      setSaving(false);
    }
  };

  const saveKeyLimits = async (keyId: string) => {
    setSaving(true);
    setError(null);
    try {
      await api.updateOrgKeyLimits(
        orgId ?? "",
        keyId,
        limitsFromDraft(keyDrafts[keyId] ?? {}, limitCurrency, rate),
      );
      const refreshed = await api.getOrgLimits(orgId ?? "");
      setData(refreshed);
    } catch (e) {
      setError(String((e as Error)?.message ?? e));
    } finally {
      setSaving(false);
    }
  };

  const spentCell = (spent: string) => (
    <span className="font-mono text-xs">{formatCost(spent)}</span>
  );

  return (
    <div className="space-y-6 p-4 sm:p-6">
      <div className="flex items-center gap-2">
        <Gauge className="h-5 w-5 text-muted-foreground" />
        <h1 className="text-lg font-semibold tracking-tight">{t("orgLimits.title")}</h1>
      </div>
      {error && <div className="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</div>}

      {/* Space level */}
      <section className="rounded-lg border p-4">
        <div className="mb-3 flex items-center justify-between">
          <h2 className="text-sm font-semibold">{t("orgLimits.spaceTitle")}</h2>
          <div className="flex gap-3 text-xs text-muted-foreground">
            {WINDOWS.map(({ key }) => (
              <span key={key}>
                {windowLabel(key)}: {spentCell(data.space.spent[key])}
              </span>
            ))}
          </div>
        </div>
        <SpendLimitsEditor
          compact
          idPrefix="space"
          draft={spaceDraft}
          currency={limitCurrency}
          onCurrencyChange={rederiveAll}
          onChange={setSpaceDraft}
          cnyPerUsd={cnyPerUsd}
          storedNano={storedNanoForEditor(data.space.limits)}
        />
      </section>

      {/* Member level */}
      <section className="rounded-lg border">
        <div className="border-b px-4 py-3">
          <h2 className="text-sm font-semibold">{t("orgLimits.membersTitle")}</h2>
        </div>
        <div className="overflow-x-auto">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{t("orgLimits.member")}</TableHead>
                <TableHead>{t("orgLimits.spentTotal")}</TableHead>
                <TableHead>{t("orgLimits.limits")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {data.members
                .filter((m) => m.role !== "owner")
                .map((m) => (
                  <TableRow key={m.user_id}>
                    <TableCell className="font-medium">
                      {m.username ?? m.user_id}
                      {m.alias ? <span className="ml-1.5 text-xs text-muted-foreground">({m.alias})</span> : null}
                    </TableCell>
                    <TableCell>{spentCell(m.spent.total_nano_usd)}</TableCell>
                    <TableCell>
                      <SpendLimitsEditor
                        compact
                        idPrefix={`member-${m.user_id}`}
                        draft={memberDrafts[m.user_id] ?? { total: "", hourly: "", daily: "" }}
                        currency={limitCurrency}
                        onCurrencyChange={rederiveAll}
                        onChange={(next) => setMemberDrafts((prev) => ({ ...prev, [m.user_id]: next }))}
                        cnyPerUsd={cnyPerUsd}
                        storedNano={storedNanoForEditor(m.limits)}
                      />
                    </TableCell>
                  </TableRow>
                ))}
            </TableBody>
          </Table>
        </div>
      </section>

      {/* Key level */}
      <section className="rounded-lg border">
        <div className="border-b px-4 py-3">
          <h2 className="text-sm font-semibold">{t("orgLimits.keysTitle")}</h2>
        </div>
        <div className="overflow-x-auto">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{t("orgLimits.keyName")}</TableHead>
                <TableHead>{t("orgLimits.creator")}</TableHead>
                <TableHead>{t("orgLimits.spentTotal")}</TableHead>
                <TableHead>{t("orgLimits.limits")}</TableHead>
                <TableHead />
              </TableRow>
            </TableHeader>
            <TableBody>
              {data.keys.map((k) => (
                <TableRow key={k.key_id}>
                  <TableCell className="font-medium">{k.name}</TableCell>
                      <TableCell className="text-xs text-muted-foreground">
                        {k.creator_username ?? k.created_by ?? "-"}
                        {k.creator_alias ? ` (${k.creator_alias})` : ""}
                      </TableCell>
                  <TableCell>{spentCell(k.spent.total_nano_usd)}</TableCell>
                  <TableCell>
                    <SpendLimitsEditor
                      compact
                      idPrefix={`key-${k.key_id}`}
                      draft={keyDrafts[k.key_id] ?? { total: "", hourly: "", daily: "" }}
                      currency={limitCurrency}
                      onCurrencyChange={rederiveAll}
                      onChange={(next) => setKeyDrafts((prev) => ({ ...prev, [k.key_id]: next }))}
                      cnyPerUsd={cnyPerUsd}
                      storedNano={storedNanoForEditor(k.limits)}
                    />
                  </TableCell>
                  <TableCell>
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={saving}
                      onClick={() => saveKeyLimits(k.key_id)}
                    >
                      {t("orgLimits.save")}
                    </Button>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </section>

      <div className="flex justify-end">
        <Button disabled={saving} onClick={save}>
          {saving ? t("common.saving") : t("orgLimits.saveSpaceAndMembers")}
        </Button>
      </div>
    </div>
  );
}
