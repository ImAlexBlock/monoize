import { Navigate, Outlet, Link, useLocation } from "react-router-dom";
import { useTranslation } from "react-i18next";
import {
  LayoutDashboard,
  Users,
  Key,
  Settings,
  Server,
  Menu,
  MessageSquareCode,
  ScrollText,
  Database,
  Store,
  CalendarClock,
  Gauge,
  HandCoins,
  Boxes,
  ShoppingBag,
  ReceiptText,
  WalletCards,
  BadgeDollarSign,
  ChartNoAxesCombined,
  BookOpenText,
  ChartSpline,
  DatabaseZap,
  HeartPulse,
  Activity,
  Building2,
  ShieldAlert,
  TrendingUp,
  Megaphone,
  Clapperboard,
} from "lucide-react";
import { useAuth } from "@/hooks/use-auth";
import { Button } from "@/components/ui/button";
import { Separator } from "@/components/ui/separator";
import { Sheet, SheetContent, SheetTrigger } from "@/components/ui/sheet";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { useState } from "react";
import { motion } from "framer-motion";
import { cn } from "@/lib/utils";
import { MonoizeLogo } from "@/components/MonoizeLogo";
import { UserCenterMenu } from "@/components/user-center-menu";
import { NotificationBell } from "@/components/announcements/notification-bell";
import { springs } from "@/components/ui/motion";
import { usePublicSiteSettings } from "@/lib/swr";
import { useMyOrgs } from "@/pages/org/shared";

const navTransition = springs.snappy;

function NavLink({
  to,
  icon: Icon,
  label,
  onClick,
  layoutId = "nav-active",
  disableLayoutAnimation = false,
  collapsed = false,
  exact = false,
  external = false,
}: {
  to: string;
  icon: React.ComponentType<{ className?: string }>;
  label: string;
  onClick?: () => void;
  layoutId?: string;
  disableLayoutAnimation?: boolean;
  collapsed?: boolean;
  exact?: boolean;
  external?: boolean;
}) {
  const location = useLocation();
  const isActive = !external
    ? exact
      ? location.pathname === to
      : location.pathname === to || location.pathname.startsWith(to + "/")
    : false;

  if (external) {
    return (
      <a
        href={to}
        target="_blank"
        rel="noreferrer"
        onClick={onClick}
        className={cn(
          "relative flex items-center rounded-md text-sm font-medium transition-colors duration-150",
          collapsed ? "justify-center px-2 py-2" : "gap-3 px-2.5 py-1.5",
          "text-muted-foreground hover:bg-accent/60 hover:text-foreground"
        )}
        title={collapsed ? label : undefined}
      >
        <span className={cn("relative z-10 flex items-center", collapsed ? "" : "gap-3")}>
          <Icon className="h-4 w-4 shrink-0" />
          {!collapsed && label}
        </span>
      </a>
    );
  }

  const link = (
    <Link
      to={to}
      onClick={onClick}
      className={cn(
        "relative flex items-center rounded-md text-sm font-medium transition-colors duration-150",
        collapsed ? "justify-center px-2 py-2" : "gap-3 px-2.5 py-1.5",
        isActive
          ? "text-foreground"
          : "text-muted-foreground hover:bg-accent/60 hover:text-foreground"
      )}
    >
      {isActive && (
        disableLayoutAnimation ? (
          <div className="absolute inset-0 rounded-md bg-accent" />
        ) : (
          <motion.div
            layoutId={layoutId}
            className="absolute inset-0 rounded-md bg-accent"
            transition={navTransition}
          />
        )
      )}
      <span className={cn("relative z-10 flex items-center", collapsed ? "" : "gap-3")}>
        <Icon className={cn("h-4 w-4 shrink-0", isActive && "text-primary")} />
        {!collapsed && label}
      </span>
    </Link>
  );

  if (collapsed) {
    return (
      <Tooltip>
        <TooltipTrigger asChild>{link}</TooltipTrigger>
        <TooltipContent side="right" sideOffset={8}>
          {label}
        </TooltipContent>
      </Tooltip>
    );
  }

  return link;
}

function Sidebar({
  onNavigate,
  layoutId = "nav-active",
  disableLayoutAnimation = false,
  collapsed = false,
}: {
  onNavigate?: () => void;
  layoutId?: string;
  disableLayoutAnimation?: boolean;
  collapsed?: boolean;
}) {
  const { user } = useAuth();
  const { t } = useTranslation();
  const isAdmin = user?.role === "super_admin" || user?.role === "admin";
  // SAU-2/SAU-4: only an eligible main account sees the sub-account page, and a
  // sub-account cannot recharge so it loses the Store entry.
  const { data: orgOverview } = useMyOrgs();
  // ORG-8a: admins, sales, and agent accounts neither create nor join spaces,
  // so they see no org entry at all; everyone else sees it as soon as they
  // belong to one or may create one.
  const hasOrgs = (orgOverview?.orgs?.length ?? 0) > 0;
  const showOrgEntries = hasOrgs || (orgOverview?.can_create ?? false);
  const inOrgMode = window.location.pathname.startsWith("/org");
  const { data: publicSite } = usePublicSiteSettings();
  const siteName = publicSite?.site_name || "LynShen Console";

  const navItems = [
    { to: "/dashboard", icon: LayoutDashboard, label: t("nav.dashboard"), exact: true },
    { to: "/dashboard/usage", icon: ChartNoAxesCombined, label: t("nav.usage") },
    { to: "/dashboard/usage/cache", icon: DatabaseZap, label: t("nav.cacheHitRate") },
    { to: "/dashboard/usage-ranking", icon: ChartSpline, label: t("nav.usageRanking") },
    { to: "/dashboard/status", icon: Activity, label: t("nav.runtimeStatus") },
    { to: "/dashboard/tokens", icon: Key, label: t("nav.apiKeys") },
    { to: "/dashboard/logs", icon: ScrollText, label: t("nav.logs") },
    { to: "/dashboard/playground", icon: MessageSquareCode, label: t("nav.playground") },
    // SB-6: opens the standalone Apeiron studio in a new tab.
    { to: "/studio-entry", icon: Clapperboard, label: t("nav.studio"), external: true },
    { to: "/dashboard/marketplace", icon: Store, label: t("nav.marketplace") },
    { to: "/dashboard/api-docs", icon: BookOpenText, label: t("nav.apiDocs") },
    { to: "/dashboard/store", icon: ShoppingBag, label: t("nav.store") },
    { to: "/dashboard/wallet", icon: WalletCards, label: t("nav.wallet") },
    { to: "/dashboard/orders", icon: ReceiptText, label: t("nav.orders") },
  ];

  // DL5c: a concise Enterprise set that still reaches Store and Orders. Store checkout is not
  // scoped by account class, so omitting them left an Enterprise user with no self-service
  // way to add balance.
  const enterpriseNavItems = [
    { to: "/dashboard/wallet", icon: WalletCards, label: t("nav.wallet") },
    { to: "/dashboard/store", icon: ShoppingBag, label: t("nav.store") },
    { to: "/dashboard/orders", icon: ReceiptText, label: t("nav.orders") },
    { to: "/dashboard/tokens", icon: Key, label: t("nav.apiKeys") },
    { to: "/dashboard/usage", icon: ChartNoAxesCombined, label: t("nav.usage") },
    { to: "/dashboard/usage/cache", icon: DatabaseZap, label: t("nav.cacheHitRate") },
    // DL5c: the Org Spaces entry is part of the Enterprise main navigation (the
    // showOrgEntries filter below hides it for users in no organization).
    { to: "/org", icon: Building2, label: t("nav.orgSpace") },
    { to: "/dashboard/logs", icon: ScrollText, label: t("nav.logs") },
    { to: "/dashboard/marketplace", icon: Store, label: t("nav.marketplace") },
    { to: "/dashboard/api-docs", icon: BookOpenText, label: t("nav.apiDocs") },
  ];
  // DL5c and DL5d: only the enterprise class gets the reduced sidebar. The private class is
  // isolated the same way enterprise is, but is a full-featured account, so it uses the
  // standard set.
  const visibleNavItems = (user?.account_class === "enterprise"
    ? enterpriseNavItems
    : navItems
  ).filter((item) => item.to !== "/org" || showOrgEntries);

  const adminNavItems = [
    { to: "/dashboard/admin", icon: Gauge, label: t("nav.adminDashboard"), exact: true },
    { to: "/dashboard/admin/runtime", icon: HeartPulse, label: t("nav.adminRuntime") },
    { to: "/dashboard/admin/revenue", icon: TrendingUp, label: t("nav.adminRevenue") },
    { to: "/dashboard/announcements-admin", icon: Megaphone, label: t("nav.announcements") },
    { to: "/dashboard/providers", icon: Server, label: t("nav.providers") },
    { to: "/dashboard/models", icon: Database, label: t("nav.models") },
    { to: "/dashboard/plans", icon: CalendarClock, label: t("nav.billingPlans") },
    { to: "/dashboard/users", icon: Users, label: t("nav.users") },
    { to: "/dashboard/groups", icon: Boxes, label: t("nav.groups") },
    { to: "/dashboard/store-admin", icon: BadgeDollarSign, label: t("nav.storeManagement") },
    { to: "/dashboard/orgs-admin", icon: Building2, label: t("nav.orgsAdmin") },
    { to: "/dashboard/orders-admin", icon: ReceiptText, label: t("nav.ordersAdmin") },
    { to: "/dashboard/sales-admin", icon: HandCoins, label: t("nav.salesManagement") },
    { to: "/dashboard/firewall", icon: ShieldAlert, label: t("nav.firewall") },
    { to: "/dashboard/admin-settings", icon: Settings, label: t("nav.settings") },
  ];

  return (
    <TooltipProvider delayDuration={0}>
      <motion.div
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        transition={{ duration: 0.2 }}
        className={cn("flex h-full flex-col p-3", collapsed ? "items-center" : "")}
      >
        <Link
          to="/"
          onClick={onNavigate}
          className={cn(
            "group flex items-center rounded-lg transition-colors hover:bg-accent/50",
            collapsed ? "justify-center p-2" : "gap-3 px-2.5 py-2.5"
          )}
        >
          <motion.div
            whileHover={{ scale: 1.05 }}
            whileTap={{ scale: 0.95 }}
            transition={springs.snappy}
            className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-foreground text-background shadow-sm"
          >
            <MonoizeLogo className="h-full w-full" />
          </motion.div>
          {!collapsed && (
            <span className="truncate font-display text-sm font-semibold tracking-tight">{siteName}</span>
          )}
        </Link>

        {showOrgEntries && (
          <div
            className={cn(
              "mt-2 flex items-center rounded-md bg-muted p-0.5",
              collapsed ? "w-full flex-col" : "mx-auto w-fit",
            )}
            role="group"
            aria-label={t("nav.modeSwitch")}
          >
            <Link
              to="/dashboard"
              relative="path"
              className={cn(
                "relative flex h-6 items-center justify-center gap-1 rounded-[5px] text-[11px] font-medium transition-colors",
                collapsed ? "w-full px-0" : "px-2",
                !inOrgMode ? "text-foreground" : "text-muted-foreground hover:text-foreground",
              )}
              title={t("nav.workspace")}
            >
              {!inOrgMode && (
                <motion.span
                  layoutId="mode-toggle-indicator"
                  className="absolute inset-0 rounded-[5px] bg-background shadow-sm"
                  transition={springs.snappy}
                />
              )}
              <LayoutDashboard className="relative z-10 size-3" />
              {!collapsed && <span className="relative z-10">{t("nav.workspace")}</span>}
            </Link>
            <Link
              to="/org"
              className={cn(
                "relative flex h-6 items-center justify-center gap-1 rounded-[5px] text-[11px] font-medium transition-colors",
                collapsed ? "w-full px-0" : "px-2",
                inOrgMode ? "text-foreground" : "text-muted-foreground hover:text-foreground",
              )}
              title={t("nav.orgSpace")}
            >
              {inOrgMode && (
                <motion.span
                  layoutId="mode-toggle-indicator"
                  className="absolute inset-0 rounded-[5px] bg-background shadow-sm"
                  transition={springs.snappy}
                />
              )}
              <Building2 className="relative z-10 size-3" />
              {!collapsed && <span className="relative z-10">{t("nav.orgSpace")}</span>}
            </Link>
          </div>
        )}

        <Separator className="my-3" />

        <nav className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto">
          {visibleNavItems.map((item) => (
            <NavLink
              key={item.to}
              {...item}
              onClick={onNavigate}
              layoutId={layoutId}
              disableLayoutAnimation={disableLayoutAnimation}
              collapsed={collapsed}
            />
          ))}

          {isAdmin && (
            <>
              <Separator className="my-2" />
              {!collapsed && (
                <p className="px-2.5 pb-1 text-xs font-medium uppercase tracking-wider text-muted-foreground">
                  {t("nav.admin")}
                </p>
              )}
              {adminNavItems.map((item) => (
                <NavLink
                  key={item.to}
                  {...item}
                  onClick={onNavigate}
                  layoutId={layoutId}
                  disableLayoutAnimation={disableLayoutAnimation}
                  collapsed={collapsed}
                />
              ))}
            </>
          )}
        </nav>

        {/* Account menu (dashboard-ui-layout.spec.md DL3a-DL3g) */}
        <div className="mt-auto pt-3">
          <Separator className="mb-3" />
          <NotificationBell collapsed={collapsed} />
          <UserCenterMenu collapsed={collapsed} onNavigate={onNavigate} />
        </div>
      </motion.div>
    </TooltipProvider>
  );
}

export function DashboardLayout() {
  const { user, loading } = useAuth();
  const { t } = useTranslation();
  const location = useLocation();
  const [open, setOpen] = useState(false);
  const [collapsed] = useState(() => {
    if (typeof window === "undefined") return false;
    return window.localStorage.getItem("lynshen-sidebar-collapsed") === "1";
  });


  if (loading) {
    return (
      <div className="flex min-h-dvh items-center justify-center bg-background">
        <motion.div
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          className="text-muted-foreground"
        >
          {t("common.loading")}
        </motion.div>
      </div>
    );
  }

  if (!user) {
    return <Navigate to="/login" replace />;
  }

  return (
    <div className="flex h-dvh overflow-hidden bg-background">
      {/* Mobile: floating menu button + sheet */}
      <Sheet open={open} onOpenChange={setOpen}>
        <SheetTrigger asChild>
          <Button
            variant="outline"
            size="icon"
            className="fixed left-4 top-4 z-50 lg:hidden"
          >
            <Menu className="h-5 w-5" />
            <span className="sr-only">Toggle menu</span>
          </Button>
        </SheetTrigger>
        <SheetContent side="left" className="w-64 border-r bg-background p-0 shadow-none">
          <Sidebar onNavigate={() => setOpen(false)} disableLayoutAnimation />
        </SheetContent>
      </Sheet>

      {/* Desktop sidebar: full-bleed, responsive collapse */}
      <motion.aside animate={{ width: collapsed ? 64 : 256 }} transition={{ duration: 0.35, ease: "easeInOut" }} className="hidden h-dvh shrink-0 border-r lg:block">
        <Sidebar collapsed={collapsed} />
        </motion.aside>

      {/* Main content area */}
      <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden px-6 py-6 pt-16 lg:px-8 lg:pt-6">
        <main
          className={cn(
            "mx-auto flex min-h-0 min-w-0 w-full flex-1 flex-col overflow-y-auto",
            // PST-U6: the dashboard status page renders dense provider grids; widen its column.
            location.pathname === "/dashboard/status" ? "max-w-[1600px]" : "max-w-6xl",
          )}
        >
          <Outlet />
        </main>
      </div>
    </div>
  );
}
