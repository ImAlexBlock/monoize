import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { ArrowRight, Boxes, CheckCircle2, CircleDollarSign, Code2, Image, MessageSquareText, Radio, ShieldCheck, Sparkles } from "lucide-react";
import { motion } from "framer-motion";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { usePublicSiteSettings } from "@/lib/swr";
import { resolvePublicApiBaseUrl, resolvePublicBrandName } from "@/lib/public-site";
import { cn } from "@/lib/utils";

const families = [
  ["responses", Sparkles],
  ["chat", MessageSquareText],
  ["messages", Code2],
  ["gemini", Radio],
  ["images", Image],
] as const;

// The section index is decorative only (PS-W4): it sits behind the heading block at low
// opacity, so it is hidden from assistive technology and never intercepts pointer events.
function SectionIntro({
  index,
  label,
  title,
  className,
  children,
}: {
  index: string;
  label: string;
  title: string;
  className?: string;
  children?: React.ReactNode;
}) {
  return (
    <div className={cn("relative isolate", className)}>
      <motion.span
        aria-hidden="true"
        initial={{ opacity: 0 }}
        whileInView={{ opacity: 1 }}
        viewport={{ once: true, amount: 0.6 }}
        transition={{ duration: 0.3 }}
        className="pointer-events-none absolute -left-2 -top-12 z-0 select-none font-display text-8xl font-semibold leading-none text-primary/10 sm:text-9xl"
      >
        {index}
      </motion.span>
      <motion.div
        initial={{ opacity: 0 }}
        whileInView={{ opacity: 1 }}
        viewport={{ once: true, amount: 0.6 }}
        transition={{ duration: 0.3 }}
        className="relative z-10"
      >
        <p className="font-mono text-xs uppercase tracking-[0.22em] text-primary">{label}</p>
        <h2 className="mt-3 font-display text-3xl font-semibold leading-tight sm:text-4xl">{title}</h2>
        {children}
      </motion.div>
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
    <div>
      <section className="relative isolate overflow-hidden border-b">
        <motion.div initial={{ opacity: 0, y: 16 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: 0.24 }} className="mx-auto grid max-w-6xl gap-12 px-4 py-20 sm:px-6 sm:py-28 lg:grid-cols-[1.15fr_0.85fr] lg:items-center lg:px-8">
          <div className="max-w-3xl">
            <p className="mb-6 font-mono text-xs font-medium uppercase tracking-[0.22em] text-primary">API GATEWAY · MODEL ROUTING</p>
            {isLoading ? <><Skeleton className="h-14 w-full max-w-xl" /><Skeleton className="mt-3 h-14 w-4/5 max-w-lg" /><Skeleton className="mt-7 h-6 w-full max-w-2xl" /></> : (
              <>
                <h1 className="font-display text-5xl font-semibold leading-[1.05] tracking-tight sm:text-7xl">{t("publicSite.welcome.title", { siteName })}</h1>
                <p className="mt-6 max-w-2xl text-lg leading-8 text-muted-foreground">{site?.site_description || t("publicSite.welcome.description", { siteName })}</p>
              </>
            )}
            <div className="mt-8 flex flex-col gap-3 sm:flex-row">
              <Button asChild size="lg" variant="primary" className="min-h-11"><Link to="/marketplace">{t("publicSite.welcome.exploreModels")}<ArrowRight /></Link></Button>
              <Button asChild size="lg" variant="outline" className="min-h-11"><Link to="/apidocs">{t("publicSite.welcome.readDocs")}</Link></Button>
            </div>
          </div>
          <Card className="overflow-hidden bg-card">
            <div className="flex items-center gap-2 border-b px-4 py-3"><span className="size-2.5 rounded-full bg-destructive/70" /><span className="size-2.5 rounded-full bg-warning/70" /><span className="size-2.5 rounded-full bg-success/70" /><span className="ml-2 font-mono text-xs text-muted-foreground">request.sh</span></div>
            <pre className="overflow-x-auto p-5 text-sm leading-7"><code>{`curl ${exampleBase}/responses \\\n  -H "Authorization: Bearer $LYNSHEN_API_KEY" \\\n  -H "Content-Type: application/json" \\\n  -d '{"model":"gpt-5","input":"Hello"}'`}</code></pre>
          </Card>
        </motion.div>
      </section>

      <section className="mx-auto max-w-6xl px-4 py-16 sm:px-6 lg:px-8">
        <SectionIntro index="01" label="API" title={t("publicSite.welcome.familiesTitle")} className="max-w-2xl">
          <p className="mt-3 text-base leading-7 text-muted-foreground">{t("publicSite.welcome.familiesDescription")}</p>
        </SectionIntro>
        <div className="mt-8 grid gap-3 sm:grid-cols-2 lg:grid-cols-5">
          {families.map(([key, Icon]) => <Card key={key} className="p-5 transition-colors duration-200 hover:border-primary/40"><Icon className="size-5 text-primary" /><h3 className="mt-5 font-semibold">{t(`publicSite.families.${key}`)}</h3></Card>)}
        </div>
      </section>

      <section className="border-y bg-muted/35">
        <div className="mx-auto grid max-w-6xl gap-8 px-4 py-16 sm:px-6 lg:grid-cols-2 lg:px-8">
          <SectionIntro index="02" label="GROUPS" title={t("publicSite.welcome.pricingTitle")}>
            <p className="mt-4 max-w-xl text-base leading-7 text-muted-foreground">{t("publicSite.welcome.pricingDescription")}</p>
          </SectionIntro>
          <div className="grid gap-4 sm:grid-cols-2"><Card className="p-6"><Boxes className="text-primary" /><h3 className="mt-4 font-semibold">{t("publicSite.welcome.groupTitle")}</h3><p className="mt-2 leading-6 text-muted-foreground">{t("publicSite.welcome.groupDescription")}</p></Card><Card className="p-6"><CircleDollarSign className="text-primary" /><h3 className="mt-4 font-semibold">{t("publicSite.welcome.priceTitle")}</h3><p className="mt-2 leading-6 text-muted-foreground">{t("publicSite.welcome.priceDescription")}</p></Card></div>
        </div>
      </section>

      <section className="mx-auto max-w-6xl px-4 py-16 sm:px-6 lg:px-8">
        <SectionIntro index="03" label="CONNECT" title={t("publicSite.welcome.stepsTitle")}>
          <p className="mt-3 text-base leading-7 text-muted-foreground">{t("publicSite.welcome.stepsDescription")}</p>
        </SectionIntro>
        <ol className="mt-8 grid gap-5 md:grid-cols-3">
          {["key", "model", "request"].map((key, index) => <li key={key} className="relative rounded-lg border bg-card p-6"><span className="font-mono text-sm text-primary">0{index + 1}</span><h3 className="mt-5 text-lg font-semibold">{t(`publicSite.steps.${key}Title`)}</h3><p className="mt-2 leading-7 text-muted-foreground">{t(`publicSite.steps.${key}Description`)}</p></li>)}
        </ol>
      </section>

      <section className="mx-auto max-w-6xl px-4 py-16 sm:px-6 lg:px-8">
        <Card className="flex flex-col gap-6 p-7 sm:flex-row sm:items-center sm:justify-between"><div className="flex gap-4"><ShieldCheck className="mt-1 size-6 shrink-0 text-success" /><div><h2 className="font-display text-2xl font-semibold">{t("publicSite.welcome.statusTitle")}</h2><p className="mt-2 text-muted-foreground">{t("publicSite.welcome.statusDescription")}</p></div></div><Button asChild variant="outline" className="min-h-11 shrink-0"><Link to="/status">{t("publicSite.welcome.viewStatus")}<CheckCircle2 /></Link></Button></Card>
      </section>
    </div>
  );
}
