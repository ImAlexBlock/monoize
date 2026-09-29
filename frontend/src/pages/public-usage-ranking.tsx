import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Activity, AlertTriangle, MousePointerClick, RefreshCw } from "lucide-react";

import { AnimatedTokenValue } from "@/components/usage/token-summary";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { EmptyState } from "@/components/ui/empty-state";
import { motion, SharedTabIndicator } from "@/components/ui/motion";
import { Skeleton } from "@/components/ui/skeleton";
import type { AdminUsageModelRow, PublicUsageRankingUserRow, UsageRankingRange } from "@/lib/api";
import { usePublicSiteSettings, usePublicUsageRanking } from "@/lib/swr";

const ranges: UsageRankingRange[] = ["today", "7d", "30d"];

function integer(value: string): bigint {
  return /^(?:0|[1-9]\d*)$/.test(value) ? BigInt(value) : 0n;
}

function totalTokens(row: Pick<AdminUsageModelRow, "input_tokens" | "cache_read_tokens" | "output_tokens">): bigint {
  return integer(row.input_tokens) + integer(row.output_tokens);
}

function RankingSkeleton() {
  return (
    <div className="mx-auto max-w-7xl px-4 py-10 sm:px-6 lg:px-8">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-end sm:justify-between">
        <div className="grid gap-3"><Skeleton className="h-10 w-56" /><Skeleton className="h-5 w-80 max-w-full" /></div>
        <Skeleton className="h-11 w-64 rounded-xl" />
      </div>
      <Skeleton className="mt-8 h-36 rounded-2xl" />
      <div className="mt-6 grid gap-6 lg:grid-cols-2"><Skeleton className="h-96 rounded-2xl" /><Skeleton className="h-96 rounded-2xl" /></div>
    </div>
  );
}

function RankingTable({
  kind,
  rows,
  onSelect,
}: {
  kind: "users" | "models";
  rows: PublicUsageRankingUserRow[] | AdminUsageModelRow[];
  onSelect?: (row: PublicUsageRankingUserRow) => void;
}) {
  const { t } = useTranslation();
  return (
    <Card className="min-w-0 overflow-hidden rounded-2xl shadow-sm">
      <CardContent className="p-0">
        <div className="border-b px-5 py-4">
          <h2 className="font-display text-lg font-semibold">
            {t(kind === "users" ? "publicSite.usageRanking.currentRanking" : "publicSite.usageRanking.modelRanking")}
          </h2>
          <p className="mt-1 text-sm text-muted-foreground">
            {t(kind === "users" ? "publicSite.usageRanking.anonymousHint" : "publicSite.usageRanking.modelHint")}
          </p>
        </div>
        {rows.length === 0 ? (
          <EmptyState title={t("publicSite.usageRanking.empty")} className="py-14" />
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full min-w-[520px] text-sm">
              <thead>
                <tr className="border-b bg-muted/35 text-left text-xs text-muted-foreground">
                  <th className="px-5 py-3 font-medium">#</th>
                  <th className="px-3 py-3 font-medium">{t(kind === "users" ? "publicSite.usageRanking.user" : "publicSite.usageRanking.model")}</th>
                  <th className="px-3 py-3 text-right font-medium">Tokens</th>
                  <th className="px-5 py-3 text-right font-medium">{t("publicSite.usageRanking.calls")}</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((row, index) => {
                  const userRow = kind === "users" ? row as PublicUsageRankingUserRow : null;
                  const modelRow = kind === "models" ? row as AdminUsageModelRow : null;
                  const key = userRow?.rank_key ?? modelRow?.model ?? String(index);
                  return (
                    <motion.tr
                      layout="position"
                      key={key}
                      transition={{ layout: { duration: 1.1, ease: [0.22, 1, 0.36, 1] } }}
                      className="border-b transition-colors duration-200 last:border-b-0 hover:bg-accent/45"
                    >
                      <td className="px-5 py-3 font-mono text-muted-foreground">{String(index + 1).padStart(2, "0")}</td>
                      <td className="max-w-56 px-3 py-3">
                        {userRow ? (
                          <button
                            type="button"
                            disabled={!userRow.models.length}
                            onClick={() => onSelect?.(userRow)}
                            className="min-h-11 w-full cursor-pointer truncate rounded-md text-left font-medium focus-visible:outline-none focus-visible:ring-[3px] focus-visible:ring-ring disabled:cursor-default"
                          >
                            {t("publicSite.usageRanking.anonymousUser")} {index + 1}
                          </button>
                        ) : (
                          <span className="block truncate font-mono">{modelRow?.model}</span>
                        )}
                      </td>
                      <td className="px-3 py-3 text-right font-mono tabular-nums">
                        <AnimatedTokenValue value={totalTokens(row)} showDelta />
                      </td>
                      <td className="px-5 py-3 text-right font-mono tabular-nums">{row.call_count.toLocaleString("en-US")}</td>
                    </motion.tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

export function PublicUsageRankingPage() {
  const { t } = useTranslation();
  const [range, setRange] = useState<UsageRankingRange>("today");
  const [selected, setSelected] = useState<PublicUsageRankingUserRow | null>(null);
  const site = usePublicSiteSettings();
  const { data, error, isLoading, mutate } = usePublicUsageRanking(range);
  const totals = useMemo(() => data ? {
    all: integer(data.total_tokens),
    input: integer(data.total_input_tokens),
    cache: integer(data.total_cache_read_tokens),
    output: integer(data.total_output_tokens),
  } : null, [data]);

  if (isLoading && !data) return <RankingSkeleton />;
  if (error && !data) {
    return (
      <div className="mx-auto max-w-7xl px-4 py-16 sm:px-6 lg:px-8">
        <EmptyState variant="card" icon={<AlertTriangle className="size-8 text-destructive" />} title={t("publicSite.usageRanking.loadFailed")} description={error instanceof Error ? error.message : t("common.error")} />
        <div className="mt-4 flex justify-center"><Button variant="outline" onClick={() => void mutate()}><RefreshCw data-icon />{t("common.retry")}</Button></div>
      </div>
    );
  }
  if (!data || !totals) return null;

  return (
    <div className="mx-auto max-w-7xl px-4 py-10 sm:px-6 lg:px-8 lg:py-14">
      <header className="flex flex-col gap-5 sm:flex-row sm:items-end sm:justify-between">
        <div className="max-w-3xl">
          <p className="font-mono text-xs font-semibold uppercase tracking-[0.18em] text-primary">{site.data?.site_name || "LynShen Console"}</p>
          <h1 className="mt-3 text-balance font-display text-3xl font-semibold tracking-tight sm:text-4xl">{t("publicSite.usageRanking.title")}</h1>
          <p className="mt-3 text-pretty text-base leading-7 text-muted-foreground">{t("publicSite.usageRanking.description")}</p>
        </div>
        <div className="relative flex min-h-11 rounded-xl border bg-card p-1 shadow-sm" role="group" aria-label={t("publicSite.usageRanking.rangeLabel")}>
          {ranges.map((item) => (
            <button key={item} type="button" aria-pressed={range === item} onClick={() => setRange(item)} className="relative min-h-9 min-w-16 cursor-pointer rounded-lg px-3 text-sm font-medium focus-visible:outline-none focus-visible:ring-[3px] focus-visible:ring-ring">
              {range === item ? <SharedTabIndicator layoutId="public-usage-range" className="absolute inset-0 rounded-lg bg-accent" /> : null}
              <span className="relative z-10">{t(`publicSite.usageRanking.ranges.${item}`)}</span>
            </button>
          ))}
        </div>
      </header>

      <motion.section key={range} initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: 0.35 }} className="mt-8 overflow-hidden rounded-2xl border bg-card shadow-sm">
        <div className="grid gap-5 p-5 sm:grid-cols-[1fr_auto] sm:items-end sm:p-6">
          <div>
            <p className="flex items-center gap-2 text-sm font-medium text-muted-foreground"><Activity className="size-4 text-primary" />{t("publicSite.usageRanking.totalTokens")}</p>
            <p className="mt-2 min-w-0 break-words font-mono text-3xl font-semibold tabular-nums sm:text-4xl"><AnimatedTokenValue value={totals.all} showDelta /></p>
          </div>
          <div className="flex items-center gap-2 text-sm text-muted-foreground"><MousePointerClick className="size-4" />{t("publicSite.usageRanking.totalCalls")}: <span className="font-mono font-semibold text-foreground">{data.total_calls.toLocaleString("en-US")}</span></div>
        </div>
        <div className="grid divide-y border-t sm:grid-cols-3 sm:divide-x sm:divide-y-0">
          {([
            ["inputTokens", totals.input, "bg-primary"],
            ["cacheTokens", totals.cache, "bg-warning"],
            ["outputTokens", totals.output, "bg-success"],
          ] as const).map(([label, value, tone]) => (
            <div key={label} className="flex items-center justify-between gap-3 p-5">
              <span className="flex items-center gap-2 text-sm text-muted-foreground"><span className={`size-2 rounded-full ${tone}`} />{t(`adminUsage.${label}`)}</span>
              <span className="font-mono text-sm font-semibold"><AnimatedTokenValue value={value} showDelta /></span>
            </div>
          ))}
        </div>
      </motion.section>

      <div className="mt-6 grid min-w-0 gap-6 lg:grid-cols-2">
        <RankingTable kind="users" rows={data.users} onSelect={setSelected} />
        <RankingTable kind="models" rows={data.models} />
      </div>

      <Dialog open={selected != null} onOpenChange={(open) => !open && setSelected(null)}>
        <DialogContent className="h-[min(38rem,calc(100dvh-2rem))] max-w-2xl rounded-2xl" closeLabel={t("common.close")}>
          <DialogHeader className="pr-10">
            <DialogTitle>{t("publicSite.usageRanking.anonymousUser")}</DialogTitle>
            <DialogDescription>{t("publicSite.usageRanking.modelDetails")}</DialogDescription>
          </DialogHeader>
          <div className="min-h-0 flex-1 overflow-auto rounded-xl border">
            <table className="w-full min-w-[520px] text-sm">
              <thead className="sticky top-0 bg-background"><tr className="border-b text-left text-xs text-muted-foreground"><th className="px-4 py-3 font-medium">{t("publicSite.usageRanking.model")}</th><th className="px-3 py-3 text-right font-medium">Tokens</th><th className="px-4 py-3 text-right font-medium">{t("publicSite.usageRanking.calls")}</th></tr></thead>
              <tbody>{selected?.models.map((model) => <tr key={model.model} className="border-b last:border-b-0"><td className="max-w-72 truncate px-4 py-3 font-mono">{model.model}</td><td className="px-3 py-3 text-right font-mono"><AnimatedTokenValue value={totalTokens(model)} /></td><td className="px-4 py-3 text-right font-mono">{model.call_count.toLocaleString("en-US")}</td></tr>)}</tbody>
            </table>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}
