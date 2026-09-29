import { useEffect, useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import {
  ArrowRight,
  BarChart3,
  CheckCircle2,
  Code2,
  Image,
  KeyRound,
  ListChecks,
  MessageSquareText,
  Radio,
  Route,
  ShieldCheck,
  Sparkles,
  Store,
  Wrench,
  WalletCards,
} from "lucide-react";
import { AnimatePresence, motion, useReducedMotion, useScroll, useTransform } from "framer-motion";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  ScrollReveal,
  ScrollStagger,
  ScrollStaggerItem,
  springs,
} from "@/components/ui/motion";
import { ModelIcon } from "@/components/ModelIcon";
import { usePublicSiteSettings } from "@/lib/swr";
import { resolvePublicApiBaseUrl } from "@/lib/public-site";
import {
  marketplaceRequest,
  type MarketplaceItem,
  type MarketplaceResponse,
} from "@/lib/marketplace-api";

const families = [
  ["responses", Sparkles],
  ["chat", MessageSquareText],
  ["messages", Code2],
  ["gemini", Radio],
  ["images", Image],
] as const;

const clientExamples = ["Claude Code", "Codex", "OpenAI SDK", "Anthropic SDK", "OpenCode"];

const advantages = [
  ["connection", KeyRound],
  ["marketplace", Store],
  ["routing", Route],
  ["records", BarChart3],
] as const;

const operations = [
  ["status", ShieldCheck],
  ["logs", ListChecks],
  ["billing", WalletCards],
] as const;

const tasks = [
  ["models", Sparkles],
  ["clients", Code2],
  ["keys", KeyRound],
  ["teams", Route],
  ["credits", WalletCards],
  ["inspect", Wrench],
] as const;

/// How many priced models the live-price strip shows at once; the window
/// rotates through the price-sorted catalog (PS-W7).
const FEATURED_CELL_COUNT = 4;
const FEATURED_ROTATION_MS = 5000;

interface FeaturedMarketplace {
  cnyPerUsd: string;
  models: MarketplaceItem[];
}

function decimalParts(value: string): [bigint, bigint] {
  const [whole, fraction = ""] = value.split(".");
  const denominator = 10n ** BigInt(fraction.length);
  return [BigInt(whole) * denominator + BigInt(fraction || "0"), denominator];
}

function compareDecimal(left: string, right: string): number {
  const [leftValue, leftScale] = decimalParts(left);
  const [rightValue, rightScale] = decimalParts(right);
  const difference = leftValue * rightScale - rightValue * leftScale;
  return difference < 0n ? -1 : difference > 0n ? 1 : 0;
}

function compareUtf8(left: string, right: string): number {
  const encoder = new TextEncoder();
  const leftBytes = encoder.encode(left);
  const rightBytes = encoder.encode(right);
  const length = Math.min(leftBytes.length, rightBytes.length);
  for (let index = 0; index < length; index += 1) {
    if (leftBytes[index] !== rightBytes[index]) return leftBytes[index]! - rightBytes[index]!;
  }
  return leftBytes.length - rightBytes.length;
}

function formatUsdPerMillion(nanoCny: string, cnyPerUsd: string): string {
  const [priceValue, priceScale] = decimalParts(nanoCny);
  const [rateValue, rateScale] = decimalParts(cnyPerUsd);
  if (rateValue <= 0n) throw new Error("exchange rate must be positive");
  const displayScale = 10_000n;
  const numerator = priceValue * rateScale * displayScale;
  const denominator = priceScale * 1_000n * rateValue;
  const rounded = (numerator * 2n + denominator) / (denominator * 2n);
  const whole = rounded / displayScale;
  const fraction = (rounded % displayScale).toString().padStart(4, "0").replace(/0+$/, "");
  return `$${whole}${fraction ? `.${fraction.padEnd(2, "0")}` : ".00"} / 1M tokens`;
}

async function loadLowPriceModels(): Promise<FeaturedMarketplace> {
  const items: MarketplaceItem[] = [];
  let cursor: string | null = null;
  let cnyPerUsd: string | null = null;
  let revision: string | null = null;

  for (let pageNumber = 0; pageNumber < 19; pageNumber += 1) {
    const query = new URLSearchParams({ limit: "50" });
    if (cursor) query.set("cursor", cursor);
    const page: MarketplaceResponse = await marketplaceRequest(
      `/api/public/marketplace?${query}`,
    );
    if (!page.cny_per_usd) throw new Error("Marketplace exchange rate is unavailable");
    if (revision !== null && (revision !== page.revision || cnyPerUsd !== page.cny_per_usd)) {
      throw new Error("Marketplace snapshot changed during pagination");
    }
    revision = page.revision;
    cnyPerUsd = page.cny_per_usd;
    items.push(...page.items);
    cursor = page.next_cursor;
    if (!cursor) break;
    if (pageNumber === 18) throw new Error("Marketplace exceeds homepage pagination limit");
  }

  const priced = items
    .filter((item) => item.input_rate_range?.unit.toLowerCase() === "token")
    .sort((left, right) => {
      const priceOrder = compareDecimal(
        left.input_rate_range!.min,
        right.input_rate_range!.min,
      );
      if (priceOrder !== 0) return priceOrder;
      const groupOrder = compareUtf8(left.public_group_name, right.public_group_name);
      return groupOrder || compareUtf8(left.model, right.model);
    });
  // Cheapest offer per model name: the list is price-sorted, so the first
  // occurrence of a model wins and later Group duplicates drop out.
  const models: MarketplaceItem[] = [];
  const seen = new Set<string>();
  for (const item of priced) {
    if (seen.has(item.model)) continue;
    seen.add(item.model);
    models.push(item);
  }
  return { cnyPerUsd: cnyPerUsd!, models };
}

function modelProviderHint(model: string): string | undefined {
  const value = model.toLowerCase();
  if (value.includes("minimax")) return "minimax";
  if (value.includes("deepseek")) return "deepseek";
  if (value.includes("glm")) return "glm";
  if (value.includes("qwen")) return "qwen";
  if (value.includes("claude")) return "anthropic";
  if (value.includes("gemini")) return "google";
  if (value.includes("gpt") || value.includes("openai")) return "openai";
  return undefined;
}

/// The mono section label with a drawing accent line: the label slides in
/// while the line grows left-to-right. Plays once on viewport entry.
function SectionKicker({ children, center = false }: { children: React.ReactNode; center?: boolean }) {
  const reduceMotion = useReducedMotion();
  return (
    <motion.p
      className={`flex items-center gap-3 font-mono text-sm text-primary${center ? " justify-center" : ""}`}
      initial={reduceMotion ? { opacity: 0 } : { opacity: 0, x: -18 }}
      whileInView={reduceMotion ? { opacity: 1 } : { opacity: 1, x: 0 }}
      viewport={{ once: true, margin: "-64px" }}
      transition={{ duration: 0.5, ease: [0.25, 1, 0.5, 1] }}
    >
      <motion.span
        aria-hidden
        className="inline-block h-px w-7 origin-left bg-primary/70"
        initial={{ scaleX: 0 }}
        whileInView={{ scaleX: 1 }}
        viewport={{ once: true, margin: "-64px" }}
        transition={{ duration: 0.55, delay: 0.1, ease: [0.25, 1, 0.5, 1] }}
      />
      {children}
    </motion.p>
  );
}

export function WelcomePage() {
  const { t } = useTranslation();
  const reduceMotion = useReducedMotion();
  const { scrollY } = useScroll();
  const heroBackdropY = useTransform(scrollY, [0, 640], [0, reduceMotion ? 0 : 130]);
  const { data: site, isLoading: siteLoading } = usePublicSiteSettings();
  const {
    data: featuredMarketplace,
    error: lowPriceError,
    isLoading: lowPriceLoading,
  } = useSWR<FeaturedMarketplace>("/api/public/marketplace?homepage=featured-usd", loadLowPriceModels);
  // PS-W7: the strip rotates through the catalog instead of pinning fixed
  // vendors; the window advances by one model every few seconds.
  const [featuredOffset, setFeaturedOffset] = useState(0);
  const featuredTotal = featuredMarketplace?.models.length ?? 0;
  useEffect(() => {
    if (featuredTotal <= FEATURED_CELL_COUNT) return;
    const timer = window.setInterval(() => {
      setFeaturedOffset((current) => (current + 1) % featuredTotal);
    }, FEATURED_ROTATION_MS);
    return () => window.clearInterval(timer);
  }, [featuredTotal]);
  const visibleFeatured = useMemo(() => {
    const list = featuredMarketplace?.models ?? [];
    if (list.length <= FEATURED_CELL_COUNT) return list;
    return Array.from({ length: FEATURED_CELL_COUNT }, (_, index) => {
      const item = list[(featuredOffset + index) % list.length]!;
      return item;
    });
  }, [featuredMarketplace, featuredOffset]);
  const siteName = site?.site_name || "LynShen Console";
  const base = resolvePublicApiBaseUrl(site?.api_base_url || "", window.location.origin);
  const exampleBase = base.baseUrl || "https://lynshen.org/v1";

  return (
    <div>
      <section className="relative isolate overflow-hidden border-b">
        <motion.div
          aria-hidden
          style={{ y: heroBackdropY }}
          className="pointer-events-none absolute inset-0 -z-10"
        >
          <div className="hero-grid-drift absolute inset-0 bg-[radial-gradient(circle_at_72%_22%,hsl(var(--primary)/0.15),transparent_34%),linear-gradient(to_right,hsl(var(--border)/0.35)_1px,transparent_1px),linear-gradient(to_bottom,hsl(var(--border)/0.35)_1px,transparent_1px)] bg-[size:auto,32px_32px,32px_32px] [mask-image:linear-gradient(to_bottom,black,transparent)]" />
          <motion.div
            className="absolute right-[16%] top-[14%] size-56 rounded-full bg-primary/10 blur-3xl motion-reduce:hidden"
            animate={reduceMotion ? undefined : { scale: [1, 1.25, 1], opacity: [0.5, 0.9, 0.5] }}
            transition={reduceMotion ? undefined : { duration: 7, repeat: Infinity, ease: "easeInOut" }}
          />
        </motion.div>
        <motion.div
          initial={{ opacity: 0, y: 16 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.24 }}
          className="mx-auto grid max-w-7xl gap-12 px-4 py-20 sm:px-6 sm:py-28 lg:grid-cols-[1.08fr_0.92fr] lg:items-center lg:px-8"
        >
          <div className="max-w-3xl">
            {siteLoading ? (
              <>
                <Skeleton className="h-14 w-full max-w-xl" />
                <Skeleton className="mt-3 h-14 w-4/5 max-w-lg" />
                <Skeleton className="mt-7 h-6 w-full max-w-2xl" />
              </>
            ) : (
              <>
                <motion.h1
                  className="whitespace-pre-line font-display text-4xl font-semibold leading-tight tracking-tight sm:text-6xl"
                  initial={reduceMotion ? { opacity: 0 } : { opacity: 0, y: 26, filter: "blur(10px)" }}
                  animate={reduceMotion ? { opacity: 1 } : { opacity: 1, y: 0, filter: "blur(0px)" }}
                  transition={{ duration: 0.7, ease: [0.25, 1, 0.5, 1] }}
                >
                  {t("publicSite.welcome.title", { siteName })}
                </motion.h1>
                <motion.p
                  className="mt-6 max-w-2xl text-lg leading-8 text-muted-foreground"
                  initial={reduceMotion ? { opacity: 0 } : { opacity: 0, y: 16 }}
                  animate={reduceMotion ? { opacity: 1 } : { opacity: 1, y: 0 }}
                  transition={{ duration: 0.6, delay: 0.18, ease: [0.25, 1, 0.5, 1] }}
                >
                  {site?.site_description || t("publicSite.welcome.description")}
                </motion.p>
              </>
            )}
            <div className="mt-8 flex flex-col gap-3 sm:flex-row">
              <motion.div
                initial={reduceMotion ? { opacity: 0 } : { opacity: 0, y: 14 }}
                animate={reduceMotion ? { opacity: 1 } : { opacity: 1, y: 0 }}
                transition={{ duration: 0.5, delay: 0.34, ease: [0.25, 1, 0.5, 1] }}
              >
                <Button asChild size="lg" variant="primary" className="group min-h-12 px-7 text-base">
                  <Link to="/dashboard">
                    {t("publicSite.welcome.enterConsole")}
                    <ArrowRight className="transition-transform duration-200 group-hover:translate-x-1 motion-reduce:transition-none motion-reduce:group-hover:translate-x-0" />
                  </Link>
                </Button>
              </motion.div>
              <motion.div
                initial={reduceMotion ? { opacity: 0 } : { opacity: 0, y: 14 }}
                animate={reduceMotion ? { opacity: 1 } : { opacity: 1, y: 0 }}
                transition={{ duration: 0.5, delay: 0.44, ease: [0.25, 1, 0.5, 1] }}
              >
                <Button asChild size="lg" variant="outline" className="min-h-12 px-7 text-base">
                  <Link to="/apidocs">{t("publicSite.welcome.readDocs")}</Link>
                </Button>
              </motion.div>
            </div>
          </div>

          <motion.div
            initial={reduceMotion ? { opacity: 0 } : { opacity: 0, y: 22, rotate: 0.6 }}
            animate={reduceMotion ? { opacity: 1 } : { opacity: 1, y: 0, rotate: 0 }}
            transition={{ duration: 0.65, delay: 0.26, ease: [0.25, 1, 0.5, 1] }}
            whileHover={reduceMotion ? undefined : { y: -4 }}
            className="overflow-hidden rounded-lg border bg-card/90 shadow-sm"
          >
            <div className="flex items-center gap-2 border-b px-4 py-3">
              <span className="terminal-dot size-2.5 rounded-full bg-destructive/70" style={{ animationDelay: "0s" }} />
              <span className="terminal-dot size-2.5 rounded-full bg-warning/70" style={{ animationDelay: "0.4s" }} />
              <span className="terminal-dot size-2.5 rounded-full bg-success/70" style={{ animationDelay: "0.8s" }} />
              <span className="ml-2 font-mono text-xs text-muted-foreground">request.sh</span>
            </div>
            <pre className="overflow-x-auto p-5 text-sm leading-7">
              <code>
                {[
                  `curl ${exampleBase}/responses \\`,
                  `  -H "Authorization: Bearer $LYNSHEN_API_KEY" \\`,
                  `  -H "Content-Type: application/json" \\`,
                  `  -d '{"model":"gpt-5","input":"Hello"}'`,
                ].map((line, index, lines) => (
                  <motion.span
                    key={index}
                    className="block"
                    initial={reduceMotion ? { opacity: 0 } : { opacity: 0, x: -10 }}
                    animate={reduceMotion ? { opacity: 1 } : { opacity: 1, x: 0 }}
                    transition={{ duration: 0.35, delay: 0.5 + index * 0.28, ease: "easeOut" }}
                  >
                    {line}
                    {index === lines.length - 1 && (
                      <span aria-hidden className="terminal-caret ml-1 inline-block h-4 w-[7px] translate-y-[3px] bg-primary/80 motion-reduce:hidden" />
                    )}
                  </motion.span>
                ))}
              </code>
            </pre>
          </motion.div>
        </motion.div>
      </section>

      <section className="border-y bg-card">
        <div className="mx-auto max-w-7xl px-4 py-20 sm:px-6 lg:px-8">
          <ScrollReveal className="mx-auto max-w-3xl text-center">
            <SectionKicker center>01 · MARKETPLACE</SectionKicker>
            <h2 className="mt-3 font-display text-3xl font-semibold sm:text-4xl">
              {t("publicSite.welcome.lowPriceTitle")}
            </h2>
            <p className="mt-4 text-lg leading-8 text-muted-foreground">
              {t("publicSite.welcome.lowPriceDescription")}
            </p>
          </ScrollReveal>

          {lowPriceLoading && !featuredMarketplace ? (
            <div className="mt-10 grid border-l border-t md:grid-cols-2 lg:grid-cols-4">
              {Array.from({ length: 4 }, (_, index) => (
                <div key={index} className="min-h-64 border-b border-r p-6">
                  <Skeleton className="h-5 w-2/3" />
                  <Skeleton className="mt-3 h-4 w-1/3" />
                  <div className="mt-10 grid grid-cols-2 gap-5">
                    <Skeleton className="h-14" />
                    <Skeleton className="h-14" />
                  </div>
                  <Skeleton className="mt-8 h-10 w-full" />
                </div>
              ))}
            </div>
          ) : lowPriceError || !featuredMarketplace ? (
            <div className="mt-10 flex flex-col items-center border-y py-10 text-center">
              <p className="text-muted-foreground">{t("publicSite.welcome.lowPriceUnavailable")}</p>
              <Button asChild size="lg" variant="outline" className="mt-5 min-h-11">
                <Link to="/marketplace">{t("publicSite.welcome.exploreModels")}<ArrowRight /></Link>
              </Button>
            </div>
          ) : (
            <div className="mt-10 grid border-l border-t md:grid-cols-2 lg:grid-cols-4">
              <AnimatePresence mode="popLayout" initial={false}>
              {visibleFeatured.map((item) => {
                const output = item.output_rate_range?.unit.toLowerCase() === "token"
                  ? formatUsdPerMillion(item.output_rate_range.min, featuredMarketplace.cnyPerUsd)
                  : "—";
                return (
                  // Keyed by model: the three carried cards glide to their new
                  // slots (`layout`), the entering card slides in from the
                  // right, and the leaving one slides out left — one spring,
                  // no whole-strip flash.
                  <motion.div
                    key={`${item.public_group_name}:${item.model}`}
                    layout={!reduceMotion}
                    initial={reduceMotion ? { opacity: 0 } : { opacity: 0, x: 56, scale: 0.97 }}
                    animate={reduceMotion ? { opacity: 1 } : { opacity: 1, x: 0, scale: 1 }}
                    exit={reduceMotion ? { opacity: 0 } : { opacity: 0, x: -56, scale: 0.97 }}
                    transition={reduceMotion ? { duration: 0.15 } : springs.smooth}
                    className="group flex min-h-64 flex-col border-b border-r p-6 transition-colors hover:bg-muted/35"
                  >
                    <div className="flex items-start gap-3">
                      <span className="flex size-10 shrink-0 items-center justify-center rounded-full border bg-background text-primary shadow-sm">
                        <ModelIcon model={item.model} provider={modelProviderHint(item.model)} className="size-6" />
                      </span>
                      <div className="min-w-0">
                        <h3 className="truncate font-mono text-lg font-semibold" title={item.model}>{item.model}</h3>
                        <p className="mt-1 flex items-center gap-1.5 text-sm text-muted-foreground">
                          <span className="truncate">{item.public_group_name}</span>
                          {item.input_rate_multiplier && (
                            <span
                              className="shrink-0 rounded bg-primary/10 px-1.5 py-0.5 font-mono text-xs text-primary"
                              title={t("publicSite.marketplace.multiplier")}
                            >
                              x{item.input_rate_multiplier}
                            </span>
                          )}
                        </p>
                      </div>
                    </div>
                    <dl className="mt-8 grid grid-cols-2 gap-5">
                      <div>
                        <dt className="text-xs text-muted-foreground">{t("publicSite.marketplace.input")}</dt>
                        <dd className="mt-2 font-mono text-xl font-semibold tabular-nums text-primary">
                          {formatUsdPerMillion(item.input_rate_range!.min, featuredMarketplace.cnyPerUsd)}
                        </dd>
                      </div>
                      <div>
                        <dt className="text-xs text-muted-foreground">{t("publicSite.marketplace.output")}</dt>
                        <dd className="mt-2 font-mono text-xl font-semibold tabular-nums">{output}</dd>
                      </div>
                    </dl>
                    <div className="mt-auto flex items-center justify-between border-t pt-4 text-sm">
                      <span className="text-muted-foreground">
                        {t("publicSite.marketplace.offerCount", { count: item.offer_count })}
                      </span>
                      <Link className="font-medium text-primary hover:underline" to="/marketplace">
                        {t("publicSite.welcome.viewPrice")}
                      </Link>
                    </div>
                  </motion.div>
                );
              })}
              </AnimatePresence>
            </div>
          )}

          <ScrollReveal delay={0.1} className="mx-auto mt-5 max-w-3xl text-center text-sm leading-6 text-muted-foreground">
            {t("publicSite.welcome.lowPriceNote")}
          </ScrollReveal>
          <ScrollReveal delay={0.15} className="mt-8 flex justify-center">
            <Button asChild size="lg" variant="outline" className="min-h-11">
              <Link to="/marketplace">{t("publicSite.welcome.exploreModels")}<ArrowRight /></Link>
            </Button>
          </ScrollReveal>
        </div>
      </section>

      <section className="mx-auto max-w-7xl px-4 py-20 sm:px-6 lg:px-8">
        <ScrollReveal className="max-w-3xl">
          <SectionKicker>02 · API</SectionKicker>
          <h2 className="mt-3 font-display text-3xl font-semibold sm:text-4xl">
            {t("publicSite.welcome.familiesTitle")}
          </h2>
          <p className="mt-4 text-lg leading-8 text-muted-foreground">
            {t("publicSite.welcome.familiesDescription")}
          </p>
        </ScrollReveal>
        <ScrollStagger className="mt-10 grid border-l border-t sm:grid-cols-2 lg:grid-cols-5">
          {families.map(([key, Icon]) => (
            <ScrollStaggerItem key={key} className="group min-h-40 border-b border-r p-5 transition-colors hover:bg-muted/30">
              <Icon className="size-5 text-primary transition-transform duration-200 group-hover:scale-110 motion-reduce:transition-none motion-reduce:group-hover:scale-100" />
              <h3 className="mt-8 font-semibold">{t(`publicSite.families.${key}`)}</h3>
            </ScrollStaggerItem>
          ))}
        </ScrollStagger>
        <ScrollReveal delay={0.1} className="grid border-x border-b lg:grid-cols-[0.35fr_0.65fr]">
          <div className="border-b p-6 lg:border-b-0 lg:border-r">
            <p className="font-mono text-xs text-primary">{t("publicSite.welcome.clientsLabel")}</p>
            <h3 className="mt-3 text-lg font-semibold">{t("publicSite.welcome.clientsTitle")}</h3>
            <p className="mt-2 leading-7 text-muted-foreground">{t("publicSite.welcome.clientsDescription")}</p>
          </div>
          <motion.div
            className="flex flex-wrap content-center gap-2 p-6"
            initial="initial"
            whileInView="show"
            viewport={{ once: true, margin: "-64px" }}
            variants={{ initial: {}, show: { transition: { staggerChildren: 0.055, delayChildren: 0.1 } } }}
          >
            {clientExamples.map((client) => (
              <motion.span
                key={client}
                className="rounded-md border bg-card px-3 py-2 font-mono text-sm transition-colors hover:border-primary/50 hover:text-primary"
                variants={
                  reduceMotion
                    ? { initial: { opacity: 0 }, show: { opacity: 1 } }
                    : { initial: { opacity: 0, scale: 0.7, rotate: -4 }, show: { opacity: 1, scale: 1, rotate: 0, transition: { type: "spring", stiffness: 380, damping: 22 } } }
                }
              >
                {client}
              </motion.span>
            ))}
          </motion.div>
        </ScrollReveal>
      </section>

      <section className="border-y bg-muted/35">
        <div className="mx-auto max-w-7xl px-4 py-20 sm:px-6 lg:px-8">
          <div className="grid gap-10 lg:grid-cols-[0.7fr_1.3fr]">
            <ScrollReveal>
              <SectionKicker>03 · RELAY</SectionKicker>
              <h2 className="mt-3 font-display text-3xl font-semibold sm:text-4xl">
                {t("publicSite.welcome.advantagesTitle")}
              </h2>
              <p className="mt-4 max-w-xl text-lg leading-8 text-muted-foreground">
                {t("publicSite.welcome.advantagesDescription")}
              </p>
            </ScrollReveal>
            <ScrollStagger className="grid border-l border-t sm:grid-cols-2">
              {advantages.map(([key, Icon]) => (
                <ScrollStaggerItem key={key} className="group border-b border-r p-6 transition-colors hover:bg-muted/30 sm:p-7">
                  <Icon className="size-5 text-primary transition-transform duration-200 group-hover:scale-110 motion-reduce:transition-none motion-reduce:group-hover:scale-100" />
                  <h3 className="mt-6 text-lg font-semibold">{t(`publicSite.advantages.${key}Title`)}</h3>
                  <p className="mt-2 leading-7 text-muted-foreground">{t(`publicSite.advantages.${key}Description`)}</p>
                </ScrollStaggerItem>
              ))}
            </ScrollStagger>
          </div>
        </div>
      </section>

      <section className="mx-auto max-w-7xl px-4 py-20 sm:px-6 lg:px-8">
        <ScrollReveal className="max-w-3xl">
          <SectionKicker>04 · WHAT YOU CAN DO</SectionKicker>
          <h2 className="mt-3 font-display text-3xl font-semibold sm:text-4xl">
            {t("publicSite.welcome.tasksTitle")}
          </h2>
          <p className="mt-4 text-lg leading-8 text-muted-foreground">
            {t("publicSite.welcome.tasksDescription")}
          </p>
        </ScrollReveal>
        <ScrollStagger className="mt-10 grid border-l border-t sm:grid-cols-2 lg:grid-cols-3">
          {tasks.map(([key, Icon]) => (
            <ScrollStaggerItem key={key} className="group min-h-48 border-b border-r p-6 transition-colors hover:bg-muted/30 sm:p-7">
              <Icon className="size-5 text-primary transition-transform duration-200 group-hover:scale-110 motion-reduce:transition-none motion-reduce:group-hover:scale-100" />
              <h3 className="mt-7 text-lg font-semibold">
                {t(`publicSite.tasks.${key}Title`)}
              </h3>
              <p className="mt-2 leading-7 text-muted-foreground">
                {t(`publicSite.tasks.${key}Description`)}
              </p>
            </ScrollStaggerItem>
          ))}
        </ScrollStagger>
      </section>

      <section className="border-y bg-muted/35">
        <div className="mx-auto max-w-7xl px-4 py-20 sm:px-6 lg:px-8">
          <ScrollReveal>
            <SectionKicker>05 · CONNECT</SectionKicker>
            <h2 className="mt-3 font-display text-3xl font-semibold sm:text-4xl">
              {t("publicSite.welcome.stepsTitle")}
            </h2>
          </ScrollReveal>
          <ScrollStagger className="mt-10 grid border-l border-t md:grid-cols-3">
            {["key", "model", "request"].map((key, index) => (
              <ScrollStaggerItem key={key} className="group min-h-56 border-b border-r p-6 transition-colors hover:bg-muted/30 sm:p-7">
                <span className="inline-block font-mono text-sm text-primary transition-transform duration-300 group-hover:-translate-y-0.5 motion-reduce:transition-none motion-reduce:group-hover:translate-y-0">0{index + 1}</span>
                <h3 className="mt-8 text-xl font-semibold">{t(`publicSite.steps.${key}Title`)}</h3>
                <p className="mt-3 leading-7 text-muted-foreground">
                  {t(`publicSite.steps.${key}Description`)}
                </p>
              </ScrollStaggerItem>
            ))}
          </ScrollStagger>
        </div>
      </section>

      <section className="mx-auto max-w-7xl px-4 py-20 sm:px-6 lg:px-8">
        <ScrollReveal className="max-w-3xl">
          <SectionKicker>06 · OPERATIONS</SectionKicker>
          <h2 className="mt-3 font-display text-3xl font-semibold sm:text-4xl">
            {t("publicSite.welcome.operationsTitle")}
          </h2>
          <p className="mt-4 text-lg leading-8 text-muted-foreground">
            {t("publicSite.welcome.operationsDescription")}
          </p>
        </ScrollReveal>
        <ScrollStagger className="mt-10 grid border-l border-t md:grid-cols-3">
          {operations.map(([key, Icon]) => (
            <ScrollStaggerItem key={key} className="group border-b border-r p-6 transition-colors hover:bg-muted/30 sm:p-7">
              <Icon className="size-5 text-primary transition-transform duration-200 group-hover:scale-110 motion-reduce:transition-none motion-reduce:group-hover:scale-100" />
              <h3 className="mt-6 text-lg font-semibold">
                {t(`publicSite.operations.${key}Title`)}
              </h3>
              <p className="mt-2 leading-7 text-muted-foreground">
                {t(`publicSite.operations.${key}Description`)}
              </p>
            </ScrollStaggerItem>
          ))}
        </ScrollStagger>
      </section>

      <section className="border-y bg-foreground text-background">
        <div className="mx-auto grid max-w-7xl gap-10 px-4 py-20 sm:px-6 lg:grid-cols-[0.72fr_1.28fr] lg:items-center lg:px-8">
          <ScrollReveal>
            <SectionKicker>07 · HTTP</SectionKicker>
            <h2 className="mt-3 font-display text-3xl font-semibold sm:text-4xl">
              {t("publicSite.welcome.codeTitle")}
            </h2>
            <p className="mt-4 text-lg leading-8 text-background/70">
              {t("publicSite.welcome.codeDescription")}
            </p>
          </ScrollReveal>
          <ScrollReveal delay={0.12} className="overflow-x-auto rounded-lg border border-background/15 bg-background/5 p-5 text-sm leading-7">
            <pre className="font-inherit"><code>{[
              "POST /v1/chat/completions HTTP/1.1",
              "Host: lynshen.org",
              "Authorization: Bearer $LYNSHEN_API_KEY",
              "Content-Type: application/json",
              "",
              `{"model":"gpt-5","messages":[{"role":"user","content":"Hello"}]}`,
            ].map((line, index) => (
              <motion.span
                key={index}
                className="block"
                initial={reduceMotion ? { opacity: 0 } : { opacity: 0, x: -12 }}
                whileInView={reduceMotion ? { opacity: 1 } : { opacity: 1, x: 0 }}
                viewport={{ once: true, margin: "-48px" }}
                transition={{ duration: 0.3, delay: index * 0.09, ease: "easeOut" }}
              >
                {line || " "}
              </motion.span>
            ))}</code></pre>
          </ScrollReveal>
        </div>
      </section>

      <section className="mx-auto max-w-7xl px-4 py-20 sm:px-6 lg:px-8">
        <div className="grid gap-8 border-y py-10 lg:grid-cols-[1fr_auto] lg:items-center">
          <ScrollReveal>
            <SectionKicker>08 · START</SectionKicker>
            <h2 className="mt-3 font-display text-3xl font-semibold">
              {t("publicSite.welcome.finalTitle")}
            </h2>
            <p className="mt-3 max-w-2xl leading-7 text-muted-foreground">
              {t("publicSite.welcome.finalDescription")}
            </p>
          </ScrollReveal>
          <ScrollReveal delay={0.12} className="flex flex-col gap-3 sm:flex-row">
            <Button asChild size="lg" variant="primary" className="min-h-12 px-6">
              <Link to="/dashboard">
                {t("publicSite.welcome.enterConsole")}
                <ArrowRight />
              </Link>
            </Button>
            <Button asChild size="lg" variant="outline" className="min-h-12 px-6">
              <Link to="/status">
                {t("publicSite.welcome.viewStatus")}
                <CheckCircle2 />
              </Link>
            </Button>
          </ScrollReveal>
        </div>
      </section>
    </div>
  );
}
