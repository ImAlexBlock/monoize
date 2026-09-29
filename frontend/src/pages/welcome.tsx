import { useEffect, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { ArrowRight, CheckCircle2 } from "lucide-react";
import { motion, useReducedMotion } from "framer-motion";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { ScrollReveal } from "@/components/ui/motion";
import { usePublicSiteSettings } from "@/lib/swr";
import { resolvePublicApiBaseUrl, resolvePublicBrandName } from "@/lib/public-site";

const modelChips = ["GPT-5", "Claude", "Gemini", "DeepSeek", "Kimi K3", "Qwen", "GLM", "Llama", "Midjourney", "Flux"];

const models = [
  { name: "GPT-5", provider: "OpenAI", descKey: "model1Desc" },
  { name: "Claude", provider: "Anthropic", descKey: "model2Desc" },
  { name: "Kimi K3", provider: "Moonshot", descKey: "model3Desc" },
] as const;

const capabilities = [1, 2, 3, 4] as const;
const faqs = [1, 2, 3] as const;

const stats: { value: number; suffix?: string; labelKey: string }[] = [
  { value: 20, labelKey: "stat1Label" },
  { value: 5, labelKey: "stat2Label" },
  { value: 99, suffix: "%", labelKey: "stat3Label" },
  { value: 24, labelKey: "stat4Label" },
];

const footerColumns = [
  { titleKey: "footerProduct", links: [["footerLinkProduct1", "/marketplace"], ["footerLinkProduct2", "/marketplace"], ["footerLinkProduct3", "/usage-ranking"]] },
  { titleKey: "footerCapabilities", links: [["footerLinkCapabilities1", "/apidocs"], ["footerLinkCapabilities2", "/apidocs"], ["footerLinkCapabilities3", "/apidocs"]] },
  { titleKey: "footerResources", links: [["footerLinkResources1", "/apidocs"], ["footerLinkResources2", "/status"], ["footerLinkResources3", "/apidocs"]] },
  { titleKey: "footerCompany", links: [["footerLinkCompany1", "/"], ["footerLinkCompany2", "/"], ["footerLinkCompany3", "/"]] },
] as const;

function Kicker({ children }: { children: React.ReactNode }) {
  return <p className="font-mono text-xs uppercase tracking-[0.16em] text-muted-foreground">{children}</p>;
}

// Count-up on viewport entry. Reduced motion renders the final value.
function Stat({ value, suffix = "", label }: { value: number; suffix?: string; label: string }) {
  const reduceMotion = useReducedMotion();
  const ref = useRef<HTMLDivElement>(null);
  const [display, setDisplay] = useState(0);

  useEffect(() => {
    if (reduceMotion) return;
    const el = ref.current;
    if (!el) return;
    const observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        const start = performance.now();
        const tick = (now: number) => {
          const progress = Math.min(1, (now - start) / 900);
          setDisplay(Math.round(value * (1 - Math.pow(1 - progress, 3))));
          if (progress < 1) requestAnimationFrame(tick);
        };
        requestAnimationFrame(tick);
        observer.unobserve(el);
      }
    }, { threshold: 0.4 });
    observer.observe(el);
    return () => observer.disconnect();
  }, [reduceMotion, value]);

  return (
    <div ref={ref} className="border-t border-border pt-6">
      <div className="text-3xl font-medium tracking-tight">{reduceMotion ? value : display}{suffix}</div>
      <div className="mt-1 text-sm text-muted-foreground">{label}</div>
    </div>
  );
}

export function WelcomePage() {
  const { t } = useTranslation();
  const { data: site, isLoading } = usePublicSiteSettings();
  const siteName = resolvePublicBrandName(site?.site_name || "LingShenAI Console");
  const base = resolvePublicApiBaseUrl(site?.api_base_url || "", window.location.origin);
  const exampleBase = base.baseUrl || "https://lynshen.org/v1";

  return (
    <div className="mx-auto max-w-5xl px-5 sm:px-8 lg:px-10">
      <header className="pb-12 pt-24 text-center sm:pt-28">
        <motion.p
          className="font-mono text-xs uppercase tracking-[0.16em] text-muted-foreground"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          transition={{ duration: 0.5 }}
        >
          API GATEWAY · MODEL ROUTING
        </motion.p>
        {isLoading ? (
          <div className="mt-6 flex flex-col items-center gap-3">
            <Skeleton className="h-12 w-full max-w-xl" />
            <Skeleton className="h-12 w-4/5 max-w-lg" />
          </div>
        ) : (
          <>
            <motion.h1
              className="mx-auto mt-5 max-w-[20ch] text-4xl font-medium leading-[1.08] tracking-tight sm:text-5xl"
              initial={{ opacity: 0, y: 18 }}
              animate={{ opacity: 1, y: 0 }}
              transition={{ duration: 0.7, ease: [0.16, 1, 0.3, 1] }}
            >
              {t("publicSite.welcome.title", { siteName })}
            </motion.h1>
            <motion.p
              className="mx-auto mt-5 max-w-[54ch] text-base leading-7 text-muted-foreground sm:text-lg"
              initial={{ opacity: 0, y: 18 }}
              animate={{ opacity: 1, y: 0 }}
              transition={{ duration: 0.7, delay: 0.12, ease: [0.16, 1, 0.3, 1] }}
            >
              {site?.site_description || t("publicSite.welcome.description", { siteName })}
            </motion.p>
          </>
        )}
        <motion.div
          className="mt-8 flex flex-col justify-center gap-3 sm:flex-row"
          initial={{ opacity: 0, y: 18 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.7, delay: 0.24, ease: [0.16, 1, 0.3, 1] }}
        >
          <Button asChild size="lg" variant="primary" className="min-h-11 rounded-full">
            <Link to="/marketplace">{t("publicSite.welcome.exploreModels")}<ArrowRight /></Link>
          </Button>
          <Button asChild size="lg" variant="outline" className="min-h-11 rounded-full">
            <Link to="/apidocs">{t("publicSite.welcome.readDocs")}</Link>
          </Button>
        </motion.div>
        <motion.p
          className="mt-4 text-sm text-muted-foreground/70"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          transition={{ duration: 0.7, delay: 0.36 }}
        >
          {t("publicSite.home.hint")}
        </motion.p>
      </header>

      <div className="home-marquee mb-6 mt-2" aria-hidden="true">
        <div className="home-marquee-track">
          {[...modelChips, ...modelChips].map((chip, index) => (
            <span key={index} className="shrink-0 rounded-full border border-border px-4 py-2 text-sm text-muted-foreground">{chip}</span>
          ))}
        </div>
      </div>

      <ScrollReveal className="overflow-hidden rounded-xl border border-border bg-card">
        <div className="flex items-center gap-2 border-b border-border px-4 py-3">
          <span className="size-2.5 rounded-full bg-muted" />
          <span className="size-2.5 rounded-full bg-muted" />
          <span className="size-2.5 rounded-full bg-muted" />
          <span className="ml-2 font-mono text-xs text-muted-foreground">request.sh</span>
        </div>
        <pre className="overflow-x-auto p-5 text-sm leading-7"><code>{`curl ${exampleBase}/responses \\\n  -H "Authorization: Bearer $API_KEY" \\\n  -H "Content-Type: application/json" \\\n  -d '{"model":"gpt-5","input":"Hello"}'`}</code></pre>
      </ScrollReveal>

      <section className="py-20 sm:py-24">
        <ScrollReveal className="max-w-2xl">
          <Kicker>{t("publicSite.home.modelsKicker")}</Kicker>
          <h2 className="mt-3 text-3xl font-medium tracking-tight sm:text-4xl">{t("publicSite.home.modelsTitle")}</h2>
          <p className="mt-3 text-base leading-7 text-muted-foreground">{t("publicSite.home.modelsDescription")}</p>
        </ScrollReveal>
        <ScrollReveal className="mt-9 border-t border-border">
          <div className="hidden grid-cols-[1.2fr_0.9fr_2fr_1.2fr] gap-5 border-b border-border py-4 font-mono text-[11px] uppercase tracking-[0.14em] text-muted-foreground sm:grid">
            <span>{t("publicSite.home.modelsColModel")}</span>
            <span>{t("publicSite.home.modelsColProvider")}</span>
            <span>{t("publicSite.home.modelsColDesc")}</span>
            <span className="text-right">{t("publicSite.home.modelsColPrice")}</span>
          </div>
          {models.map((model) => (
            <div key={model.name} className="grid gap-x-5 gap-y-1 border-b border-border py-4 transition-colors hover:bg-muted/30 sm:grid-cols-[1.2fr_0.9fr_2fr_1.2fr] sm:items-baseline">
              <span className="text-base font-medium tracking-tight">{model.name}</span>
              <span className="text-sm text-muted-foreground">{model.provider}</span>
              <span className="text-sm text-muted-foreground">{t(`publicSite.home.${model.descKey}`)}</span>
              <span className="font-mono text-[13px] text-muted-foreground sm:text-right">{t("publicSite.home.pricePlaceholder")}</span>
            </div>
          ))}
          <Link to="/marketplace" className="mt-5 inline-block text-sm text-muted-foreground transition-colors hover:text-foreground">{t("publicSite.home.modelsMore")}</Link>
        </ScrollReveal>
      </section>

      <section className="pb-20 sm:pb-24">
        <ScrollReveal className="max-w-2xl">
          <Kicker>{t("publicSite.home.capKicker")}</Kicker>
          <h2 className="mt-3 text-3xl font-medium tracking-tight sm:text-4xl">{t("publicSite.home.capTitle")}</h2>
        </ScrollReveal>
        <div className="mt-9">
          {capabilities.map((index) => (
            <ScrollReveal key={index} className="grid grid-cols-[40px_1fr] items-baseline gap-6 border-t border-border py-8 sm:grid-cols-[56px_1fr_1.5fr]">
              <span className="font-mono text-xs tracking-[0.14em] text-muted-foreground">0{index}</span>
              <h3 className="text-lg font-medium tracking-tight">{t(`publicSite.home.cap${index}Title`)}</h3>
              <p className="col-start-2 text-[15px] text-muted-foreground sm:col-start-3">{t(`publicSite.home.cap${index}Desc`)}</p>
            </ScrollReveal>
          ))}
        </div>
      </section>

      <div className="grid grid-cols-2 gap-6 border-b border-border pb-10 sm:grid-cols-4">
        {stats.map((stat) => <Stat key={stat.labelKey} value={stat.value} suffix={stat.suffix} label={t(`publicSite.home.${stat.labelKey}`)} />)}
      </div>

      <section className="py-20 sm:py-24">
        <ScrollReveal className="max-w-2xl">
          <Kicker>{t("publicSite.home.faqKicker")}</Kicker>
          <h2 className="mt-3 text-3xl font-medium tracking-tight sm:text-4xl">{t("publicSite.home.faqTitle")}</h2>
        </ScrollReveal>
        <div className="mt-9">
          {faqs.map((index) => (
            <details key={index} className="home-faq group border-t border-border py-5">
              <summary className="flex cursor-pointer list-none items-center justify-between text-base font-medium">
                {t(`publicSite.home.faq${index}Q`)}
                <span className="text-muted-foreground transition-transform duration-200 group-open:rotate-45">+</span>
              </summary>
              <p className="home-faq-body pt-3 text-[15px] text-muted-foreground">{t(`publicSite.home.faq${index}A`)}</p>
            </details>
          ))}
        </div>
      </section>

      <section className="py-24 text-center">
        <ScrollReveal>
          <h2 className="mx-auto max-w-2xl text-3xl font-medium tracking-tight sm:text-5xl">{t("publicSite.home.ctaTitle")}</h2>
          <div className="mt-7 flex flex-col justify-center gap-3 sm:flex-row">
            <Button asChild size="lg" variant="primary" className="min-h-11 rounded-full">
              <Link to="/marketplace">{t("publicSite.welcome.exploreModels")}<ArrowRight /></Link>
            </Button>
            <Button asChild size="lg" variant="outline" className="min-h-11 rounded-full">
              <Link to="/status">{t("publicSite.welcome.viewStatus")}<CheckCircle2 /></Link>
            </Button>
          </div>
        </ScrollReveal>
      </section>

      <ScrollReveal className="border-t border-border py-12">
        <div className="grid grid-cols-2 gap-6 sm:grid-cols-4">
          {footerColumns.map((column) => (
            <div key={column.titleKey}>
              <h4 className="mb-3 text-sm font-medium">{t(`publicSite.home.${column.titleKey}`)}</h4>
              {column.links.map(([labelKey, to]) => (
                <Link key={labelKey} to={to} className="block py-1 text-sm text-muted-foreground transition-colors hover:text-foreground">{t(`publicSite.home.${labelKey}`)}</Link>
              ))}
            </div>
          ))}
        </div>
      </ScrollReveal>
    </div>
  );
}
