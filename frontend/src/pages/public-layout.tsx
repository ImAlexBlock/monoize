import { useState } from "react";
import { Link, NavLink, Outlet } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { Github, Languages, Menu, Moon, Sun, X } from "lucide-react";
import { MonoizeLogo } from "@/components/MonoizeLogo";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { useTheme } from "@/hooks/use-theme";
import { toggleLanguage } from "@/i18n";
import { usePublicSiteSettings } from "@/lib/swr";
import { cn } from "@/lib/utils";

const links = [
  ["/", "home"],
  ["/marketplace", "marketplace"],
  ["/usage-ranking", "usageRanking"],
  ["/apidocs", "apiDocs"],
  ["/status", "status"],
] as const;

export function PublicLayout() {
  const { t } = useTranslation();
  const { data: site, isLoading } = usePublicSiteSettings();
  const { resolvedTheme, setTheme } = useTheme();
  const [menuOpen, setMenuOpen] = useState(false);
  const siteName = site?.site_name || "LynShen Console";

  return (
    <div className="h-dvh overflow-y-auto overflow-x-hidden bg-background text-foreground">
      <a
        href="#public-content"
        className="fixed left-4 top-2 z-[60] -translate-y-20 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground focus:translate-y-0 focus:outline-none focus:ring-[3px] focus:ring-ring"
      >
        {t("publicSite.skipToContent")}
      </a>
      <header className="sticky top-0 z-40 border-b bg-background/92 backdrop-blur-xl">
        <nav className="mx-auto flex min-h-16 max-w-7xl items-center gap-3 px-4 sm:px-6 lg:px-8" aria-label={t("publicSite.primaryNavigation")}>
          <Link to="/" className="flex min-h-11 min-w-0 items-center gap-3 rounded-md focus-visible:outline-none focus-visible:ring-[3px] focus-visible:ring-ring">
            <span className="flex size-9 shrink-0 items-center justify-center rounded-lg border bg-card p-1.5 text-foreground">
              <MonoizeLogo className="size-full" />
            </span>
            {isLoading ? (
              <Skeleton className="h-5 w-32" />
            ) : (
              <span className="truncate font-display text-base font-semibold tracking-tight">{siteName}</span>
            )}
          </Link>

          <div className="ml-auto hidden items-center gap-1 lg:flex">
            {links.map(([to, key]) => (
              <NavLink
                key={to}
                to={to}
                end={to === "/"}
                className={({ isActive }) => cn(
                  "flex min-h-11 items-center rounded-md px-3 text-sm font-medium transition-colors duration-200 focus-visible:outline-none focus-visible:ring-[3px] focus-visible:ring-ring",
                  isActive ? "bg-accent text-foreground" : "text-muted-foreground hover:bg-accent/70 hover:text-foreground",
                )}
              >
                {t(`publicSite.nav.${key}`)}
              </NavLink>
            ))}
            <Button
              asChild
              variant="ghost"
              size="icon"
              className="size-11"
              aria-label="GitHub"
              title="GitHub"
            >
              <a href="https://github.com/Ikaleio/monoize" target="_blank" rel="noreferrer noopener">
                <Github />
              </a>
            </Button>
            <Button variant="ghost" size="icon" className="size-11" onClick={toggleLanguage} aria-label={t("language.switchLanguage")}>
              <Languages />
            </Button>
            <Button
              variant="ghost"
              size="icon"
              className="size-11"
              onClick={() => setTheme(resolvedTheme === "dark" ? "light" : "dark")}
              aria-label={t("theme.toggle")}
            >
              {resolvedTheme === "dark" ? <Moon /> : <Sun />}
            </Button>
            <Button asChild variant="outline" className="min-h-11">
              <Link to="/login">{t("publicSite.nav.login")}</Link>
            </Button>
            <Button asChild variant="primary" className="min-h-11">
              <Link to="/dashboard">{t("publicSite.nav.console")}</Link>
            </Button>
          </div>

          <Button
            variant="outline"
            size="icon"
            className="ml-auto size-11 lg:hidden"
            onClick={() => setMenuOpen((open) => !open)}
            aria-expanded={menuOpen}
            aria-controls="public-mobile-menu"
            aria-label={t("publicSite.nav.menu")}
          >
            {menuOpen ? <X /> : <Menu />}
          </Button>
        </nav>
        {menuOpen && (
          <div id="public-mobile-menu" className="border-t bg-background px-4 py-3 lg:hidden">
            <div className="mx-auto grid max-w-7xl gap-1">
              {links.map(([to, key]) => (
                <NavLink key={to} to={to} end={to === "/"} onClick={() => setMenuOpen(false)} className="flex min-h-11 items-center rounded-md px-3 text-base font-medium hover:bg-accent focus-visible:outline-none focus-visible:ring-[3px] focus-visible:ring-ring">
                  {t(`publicSite.nav.${key}`)}
                </NavLink>
              ))}
              <div className="mt-2 grid grid-cols-2 gap-2 border-t pt-3">
                <Button variant="outline" className="min-h-11" onClick={toggleLanguage}><Languages />{t("publicSite.nav.language")}</Button>
                <Button variant="outline" className="min-h-11" onClick={() => setTheme(resolvedTheme === "dark" ? "light" : "dark")}>{resolvedTheme === "dark" ? <Moon /> : <Sun />}{t("publicSite.nav.theme")}</Button>
                <Button asChild variant="outline" className="min-h-11"><Link to="/login">{t("publicSite.nav.login")}</Link></Button>
                <Button asChild variant="primary" className="min-h-11"><Link to="/dashboard">{t("publicSite.nav.console")}</Link></Button>
              </div>
            </div>
          </div>
        )}
      </header>

      <main id="public-content" tabIndex={-1} className="outline-none">
        <Outlet />
      </main>
      <footer className="border-t">
        <div className="mx-auto flex max-w-7xl flex-col gap-2 px-4 py-8 text-sm text-muted-foreground sm:flex-row sm:items-center sm:justify-between sm:px-6 lg:px-8">
          <span>{siteName}</span>
          <span className="flex items-center gap-3">
            {t("publicSite.footer")}
            <a
              href="https://github.com/Ikaleio/monoize"
              target="_blank"
              rel="noreferrer noopener"
              aria-label="GitHub"
              className="inline-flex size-8 items-center justify-center rounded-md transition-colors hover:bg-accent hover:text-foreground"
            >
              <Github className="size-4" />
            </a>
          </span>
        </div>
      </footer>
    </div>
  );
}
