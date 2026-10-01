import { useMemo, useState } from "react";
import { ArrowDownLeft, ArrowUpRight, Infinity as InfinityIcon, WalletCards, RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";
import useSWR from "swr";
import { Card, CardContent } from "@/components/ui/card";
import { PageHeader } from "@/components/ui/page-header";
import { Skeleton } from "@/components/ui/skeleton";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { CoinAmount } from "@/components/coin-amount";
import { PageWrapper } from "@/components/ui/motion";
import { useAuth } from "@/hooks/use-auth";
import { useStoreExchangeRate } from "@/hooks/use-store-exchange-rate";
import { useStoreCurrency } from "@/hooks/use-store-currency";
import { api, type BillingLedgerEntry } from "@/lib/api";
import { storeApi } from "@/lib/store-api";
import { formatCoinFromNanoUsdForCurrency } from "@/lib/store-money";
import { RedemptionPanel } from "./store/redemption-panel";
import { OrdersPage } from "./orders";

const ENTITLEMENT_KEY = "/api/dashboard/store/entitlement";

export function WalletPage() {
  const { t } = useTranslation();
  const { user, refreshUser, loading: userLoading } = useAuth();
  const [redeeming, setRedeeming] = useState(false);
  const [walletSection, setWalletSection] = useState<"ledger" | "orders">("ledger");
  const [ledgerLimit, setLedgerLimit] = useState(10);
  const exchangeRate = useStoreExchangeRate(true);
  const { currency } = useStoreCurrency();
  const entitlement = useSWR(ENTITLEMENT_KEY, storeApi.getEntitlement);
  const monthStart = useMemo(() => {
    const now = new Date();
    return new Date(now.getFullYear(), now.getMonth(), 1).toISOString();
  }, []);
  const monthlyUsage = useSWR(["wallet-monthly-usage", monthStart], () => api.listRequestLogs(1, 0, { time_from: monthStart }));
  const ledger = useSWR(["/api/dashboard/wallet/ledger", ledgerLimit], () => api.listWalletLedger(ledgerLimit));
  const summaryError = monthlyUsage.error || entitlement.error || exchangeRate.error;
  const summaryLoading = userLoading || monthlyUsage.isLoading || entitlement.isLoading || exchangeRate.isLoading;
  const rate = exchangeRate.data?.cny_per_usd;
  const formatCoin = (value: string | null | undefined) => rate && value != null ? formatCoinFromNanoUsdForCurrency(value, currency, rate) : "--";
  const ledgerLabel = (entry: BillingLedgerEntry) => t(`wallet.ledger.kinds.${entry.kind}`, { defaultValue: entry.kind });

  const handleRedeem = async (code: string) => {
    setRedeeming(true);
    try {
      await storeApi.redeem(code);
      await Promise.all([refreshUser(), entitlement.mutate()]);
    } finally {
      setRedeeming(false);
    }
  };

  return (
    <PageWrapper className="flex min-w-0 flex-col gap-6 pb-6">
      <PageHeader title={t("wallet.title")} description={t("wallet.description")} />
      {summaryError ? (
        <EmptyState variant="inline" role="alert" title={t("common.error")}
          action={<Button variant="outline" onClick={() => {
            void monthlyUsage.mutate();
            void entitlement.mutate();
            void exchangeRate.mutate();
          }}><RefreshCw className="mr-2 h-4 w-4" />{t("common.retry")}</Button>} />
      ) : summaryLoading ? (
        <div className="grid gap-3 sm:grid-cols-3" aria-busy="true" aria-label={t("wallet.summaryLabel")}>
          {[0, 1, 2].map((item) => <Skeleton key={item} className="h-24" />)}
        </div>
      ) : (
      <section className="grid gap-3 sm:grid-cols-3" aria-label={t("wallet.summaryLabel")}>
        <Card className="rounded-2xl"><CardContent className="p-5"><p className="text-sm text-muted-foreground">{t("wallet.balance")}</p><p className="mt-2 text-xl font-semibold">{user?.balance_unlimited ? <span className="flex items-center gap-2"><InfinityIcon className="size-5" />{t("store.ui.accountUnlimited")}</span> : <CoinAmount value={formatCoin(user?.balance_nano_usd)} iconClassName="size-5" />}</p></CardContent></Card>
        <Card className="rounded-2xl"><CardContent className="p-5"><p className="text-sm text-muted-foreground">{t("wallet.monthlyUsage")}</p><p className="mt-2 text-xl font-semibold"><CoinAmount value={formatCoin(monthlyUsage.data?.total_charge_nano_usd)} iconClassName="size-5" /></p></CardContent></Card>
        <Card className="rounded-2xl"><CardContent className="p-5"><p className="text-sm text-muted-foreground">{t("wallet.currentPlan")}</p><p className="mt-2 font-semibold">{entitlement.data?.product_name ?? t("store.account.noPlan")}</p>{entitlement.data && <p className="mt-1 text-xs text-muted-foreground">{t("store.ui.accountEnds", { date: new Date(entitlement.data.ends_at).toLocaleDateString() })}</p>}</CardContent></Card>
      </section>
      )}
      <Card className="rounded-2xl"><CardContent className="p-5"><div className="mb-4 flex items-center gap-2"><WalletCards className="size-5 text-primary" /><h2 className="font-display text-base font-semibold">{t("wallet.redemptionTitle")}</h2></div><RedemptionPanel onRedeem={handleRedeem} redeeming={redeeming} /></CardContent></Card>
      <section aria-labelledby="wallet-records-title">
        <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-3"><h2 id="wallet-records-title" className="font-display text-lg font-semibold">{t("wallet.recordsTitle")}</h2>{walletSection === "ledger" ? <label className="flex items-center gap-2 text-xs text-muted-foreground"><span>{t("wallet.ledger.limit")}</span><select value={ledgerLimit} onChange={(event) => setLedgerLimit(Number(event.target.value))} className="h-9 rounded-lg border bg-background px-2 text-foreground"><option value="10">10</option><option value="25">25</option><option value="50">50</option></select></label> : null}</div>
          <div className="grid grid-cols-2 rounded-xl bg-muted p-1" role="tablist" aria-label={t("wallet.recordsTitle")}>
            {(["ledger", "orders"] as const).map((section) => (
              <button key={section} type="button" role="tab" aria-selected={walletSection === section} onClick={() => setWalletSection(section)} className={`min-h-10 rounded-lg px-4 text-sm font-medium transition-colors ${walletSection === section ? "bg-background text-foreground shadow-sm" : "text-muted-foreground"}`}>
                {t(`wallet.tabs.${section}`)}
              </button>
            ))}
          </div>
        </div>
        {walletSection === "orders" ? <OrdersPage embedded /> : (
          ledger.isLoading ? <div className="grid gap-2" aria-hidden="true">{[0, 1, 2].map((item) => <Skeleton key={item} className="h-16 rounded-xl" />)}</div>
            : ledger.error ? <Card className="rounded-2xl border-destructive/40"><CardContent className="p-6 text-sm text-destructive">{t("wallet.ledger.loadFailed")}</CardContent></Card>
              : !ledger.data?.length ? <Card className="rounded-2xl"><CardContent className="p-8 text-center text-sm text-muted-foreground">{t("wallet.ledger.empty")}</CardContent></Card>
                : <div className="divide-y overflow-hidden rounded-2xl border bg-card">{ledger.data.map((entry) => <LedgerRow key={entry.id} entry={entry} label={ledgerLabel(entry)} rate={rate} currency={currency} />)}</div>
        )}
      </section>
    </PageWrapper>
  );
}

function LedgerRow({ entry, label, rate, currency }: { entry: BillingLedgerEntry; label: string; rate?: string; currency: "CNY" | "USD" }) {
  const positive = !entry.delta_nano_usd.startsWith("-") && entry.delta_nano_usd !== "0";
  const value = rate ? formatCoinFromNanoUsdForCurrency(entry.delta_nano_usd, currency, rate) : "--";
  const meta = typeof entry.meta?.model === "string" ? entry.meta.model : typeof entry.meta?.order_id === "string" ? `#${entry.meta.order_id}` : null;
  return (
    <div className="flex items-center justify-between gap-4 px-4 py-4 sm:px-5">
      <div className="flex min-w-0 items-center gap-3">
        <span className={`grid size-9 shrink-0 place-items-center rounded-full ${positive ? "bg-emerald-500/10 text-emerald-600" : "bg-rose-500/10 text-rose-600"}`}>
          {positive ? <ArrowUpRight className="size-4" /> : <ArrowDownLeft className="size-4" />}
        </span>
        <div className="min-w-0"><p className="truncate text-sm font-medium">{label}</p><p className="truncate text-xs text-muted-foreground">{meta ?? new Date(entry.created_at).toLocaleString()}</p></div>
      </div>
      <div className="shrink-0 text-right"><CoinAmount value={value} className={`font-mono text-sm font-semibold ${positive ? "text-emerald-600" : "text-rose-600"}`} /><p className="mt-1 text-xs text-muted-foreground">{new Date(entry.created_at).toLocaleString()}</p></div>
    </div>
  );
}
