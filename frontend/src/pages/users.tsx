import { useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { Plus, Trash2, Pencil, Search, Shield, ShieldCheck, User as UserIcon, Mail, PlusCircle, ScrollText } from "lucide-react";
import { GroupsBadge } from "@/components/GroupsBadge";
import { GroupSingleSelect } from "@/components/groups/GroupPicker";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { TableVirtuoso } from "react-virtuoso";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { useAuth } from "@/hooks/use-auth";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  useUsers,
  useDashboardGroups,
  useBillingPlans,
  createUserOptimistic,
  updateUserOptimistic,
  deleteUserOptimistic,
  updateUserAccountClassOptimistic,
} from "@/lib/swr";
import type { AccountClass, User } from "@/lib/api";
import { formatNanoUsd, formatUsdDecimal, isSignedIntegerString } from "@/lib/exact-decimal";
import { useStoreCurrency } from "@/hooks/use-store-currency";
import { useStoreExchangeRate } from "@/hooks/use-store-exchange-rate";
import { formatCoinFromNanoUsdExact, formatCoinFromNanoUsdForCurrency } from "@/lib/store-money";
import { Avatar, AvatarImage, AvatarFallback } from "@/components/ui/avatar";
import { getGravatarUrl } from "@/lib/utils";
import { AnimatedButton, PageWrapper, motion, transitions } from "@/components/ui/motion";
import { PageHeader } from "@/components/ui/page-header";
import { TablePageSkeleton } from "@/components/ui/page-skeleton";
import { DataTableShell, VirtualTableCell, VirtualTableHeaderCell } from "@/components/ui/data-table-shell";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "sonner";

const NANO_PER_USD = 1_000_000_000n;

function parseUsdToNanoBigInt(usd: string): bigint | null {
  const trimmed = usd.trim();
  if (!trimmed || trimmed === "-") return null;

  const negative = trimmed.startsWith("-");
  const abs = negative ? trimmed.slice(1) : trimmed;
  const parts = abs.split(".");
  if (parts.length > 2) return null;

  const intPart = parts[0] || "0";
  let fracPart = parts[1] || "";
  if (fracPart.length > 9) fracPart = fracPart.slice(0, 9);
  fracPart = fracPart.padEnd(9, "0");

  try {
    const nano = BigInt(intPart) * NANO_PER_USD + BigInt(fracPart);
    return negative ? -nano : nano;
  } catch {
    return null;
  }
}

/**
 * Converts a typed amount to nano-USD in the selected add currency. A CNY amount
 * divides by the live cny_per_usd snapshot with BigInt-only arithmetic, so the
 * credited value matches what a CNY top-up of that amount buys.
 */
function amountToNanoForCurrency(
  amount: string,
  currency: "USD" | "CNY",
  cnyPerUsd: string | undefined
): bigint | null {
  const nano = parseUsdToNanoBigInt(amount);
  if (nano === null) return null;
  if (currency === "USD") return nano;
  const rate = Number(cnyPerUsd);
  if (!Number.isFinite(rate) || rate <= 0) return null;
  const scaledRate = BigInt(Math.round(rate * 1e9));
  const numerator = nano * 1_000_000_000n;
  const quotient = numerator / scaledRate;
  const remainder = numerator % scaledRate;
  const doubled = remainder * 2n;
  const roundUp = nano < 0n ? doubled <= -scaledRate : doubled >= scaledRate;
  return roundUp ? quotient + (nano < 0n ? -1n : 1n) : quotient;
}

function nanoToUsdString(nano: bigint): string {
  const negative = nano < 0n;
  const abs = negative ? -nano : nano;
  const intPart = abs / NANO_PER_USD;
  const fracPart = abs % NANO_PER_USD;
  const fracStr = fracPart.toString().padStart(9, "0").replace(/0+$/, "");
  const result = fracStr ? `${intPart}.${fracStr}` : `${intPart}`;
  return negative ? `-${result}` : result;
}

const roleIcons = {
  super_admin: ShieldCheck,
  admin: Shield,
  user: UserIcon,
};

const roleVariants = {
  super_admin: "destructive" as const,
  admin: "default" as const,
  user: "secondary" as const,
};

/**
 * Admin list groupings (UM-S1).
 *
 * The first three mirror the account classes. Sales agents are ordinary standard-class user
 * accounts, so without a grouping of their own they sit indistinguishable among the standard
 * users. This grouping is presentational only: it changes no permission, class, or route.
 */
const USER_SCOPES = ["standard", "enterprise", "private", "agent", "sales"] as const;

type UserScope = (typeof USER_SCOPES)[number];

/** Assigns a user to exactly one grouping, with agents taking precedence over their class. */
function scopeOf(user: User): UserScope {
  if (user.is_sales_agent) return "sales";
  return user.account_class;
}

export function UsersPage() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const { currency } = useStoreCurrency();
  const { data: exchangeRate } = useStoreExchangeRate();
  const { user: currentUser } = useAuth();
  const { data: users = [], isLoading } = useUsers();
  const [scope, setScope] = useState<UserScope>("standard");
  const { data: groups = [], isLoading: groupsLoading } = useDashboardGroups();
  const { data: billingPlans = [] } = useBillingPlans();
  const defaultGroupId = useMemo(
    () => groups.find((group) => group.is_default)?.id ?? "",
    [groups]
  );
  const scopedUsers = useMemo(
    () => users.filter((user) => scopeOf(user) === scope),
    [users, scope],
  );
  const [search, setSearch] = useState("");
  const normalizedSearch = search.trim().toLowerCase();
  const visibleUsers = useMemo(
    () =>
      scopedUsers.filter(
        (user) =>
          !normalizedSearch ||
          user.username.toLowerCase().includes(normalizedSearch) ||
          (user.email ?? "").toLowerCase().includes(normalizedSearch),
      ),
    [scopedUsers, normalizedSearch],
  );
  const scopeCounts = useMemo(() => {
    const counts: Record<UserScope, number> = {
      standard: 0,
      enterprise: 0,
      private: 0,
      agent: 0,
      sales: 0,
    };
    for (const user of users) counts[scopeOf(user)] += 1;
    return counts;
  }, [users]);

  const todayTotals = useMemo(() => {
    let calls = 0;
    let cost = 0n;
    for (const user of scopedUsers) {
      calls += user.today_calls ?? 0;
      const raw = user.today_cost_nano_usd ?? "0";
      if (isSignedIntegerString(raw)) {
        cost += BigInt(raw);
      }
    }
    return { calls, cost };
  }, [scopedUsers]);
  const [createOpen, setCreateOpen] = useState(false);
  const [editUser, setEditUser] = useState<User | null>(null);
  const [formData, setFormData] = useState({
    username: "",
    password: "",
    role: "user",
    balanceUsd: "0",
    balanceUnlimited: false,
    email: "",
    groupId: "",
    billingPlanId: "" as string,
  });
  const [balanceMode, setBalanceMode] = useState<"set" | "add">("set");
  const [balanceAddAmount, setBalanceAddAmount] = useState("");
  /// The add-amount currency: the wallet is nano-USD, so a CNY amount converts
  /// through the live exchange-rate snapshot before it is applied.
  const [balanceAddCurrency, setBalanceAddCurrency] = useState<"USD" | "CNY">("CNY");
  const [saving, setSaving] = useState(false);
  const [deleteTargetId, setDeleteTargetId] = useState<string | null>(null);
  const [accountClassTarget, setAccountClassTarget] = useState<{
    user: User;
    next: AccountClass;
  } | null>(null);

  const handleCreate = async () => {
    if (!formData.username.trim() || !formData.password) return;
    setSaving(true);
    try {
      await createUserOptimistic(
        formData.username.trim(),
        formData.password,
        formData.role,
        formData.groupId || undefined,
        users
      );
      setCreateOpen(false);
      setFormData({
        username: "",
        password: "",
        role: "user",
        balanceUsd: "0",
        balanceUnlimited: false,
        email: "",
        groupId: "",
        billingPlanId: "",
      });
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t("users.failedCreate"));
    } finally {
      setSaving(false);
    }
  };

  const handleUpdate = async () => {
    if (!editUser) return;
    setSaving(true);
    try {
      const updates: {
        username?: string;
        password?: string;
        role?: User["role"];
        balance_usd?: string;
        balance_nano_usd?: string;
        balance_unlimited?: boolean;
        email?: string | null;
        group_id?: string;
        billing_plan_id?: string | null;
      } = {};
      if (formData.username.trim() && formData.username !== editUser.username) {
        updates.username = formData.username.trim();
      }
      if (formData.password) {
        updates.password = formData.password;
      }
      if (formData.role !== editUser.role) {
        updates.role = formData.role as User["role"];
      }
      if (balanceMode === "add") {
        const addNano =
          balanceAddCurrency === "USD"
            ? parseUsdToNanoBigInt(balanceAddAmount)
            : amountToNanoForCurrency(
                balanceAddAmount,
                balanceAddCurrency,
                exchangeRate?.cny_per_usd
              );
        if (addNano === null && balanceAddAmount.trim()) {
          toast.error(
            balanceAddCurrency === "CNY" && !exchangeRate?.cny_per_usd
              ? t("users.balanceAddNoRate")
              : t("users.balanceAddInvalid")
          );
          setSaving(false);
          return;
        }
        if (addNano !== null && addNano !== 0n) {
          const newNano = BigInt(editUser.balance_nano_usd) + addNano;
          updates.balance_nano_usd = newNano.toString();
        }
      } else if (formData.balanceUsd !== editUser.balance_usd) {
        updates.balance_usd = formData.balanceUsd.trim();
      }
      if (formData.balanceUnlimited !== editUser.balance_unlimited) {
        updates.balance_unlimited = formData.balanceUnlimited;
      }
      const trimmedEmail = formData.email.trim();
      const currentEmail = editUser.email ?? "";
      if (trimmedEmail !== currentEmail) {
        updates.email = trimmedEmail || null;
      }
      if (formData.groupId && formData.groupId !== editUser.group_id) {
        updates.group_id = formData.groupId;
      }
      const nextPlanId =
        !formData.billingPlanId || formData.billingPlanId === "none"
          ? null
          : formData.billingPlanId;
      if (nextPlanId !== (editUser.billing_plan_id ?? null)) {
        updates.billing_plan_id = nextPlanId;
      }
      await updateUserOptimistic(
        editUser.id,
        updates,
        users
      );
      setEditUser(null);
      setBalanceMode("set");
      setBalanceAddAmount("");
      setFormData({
        username: "",
        password: "",
        role: "user",
        balanceUsd: "0",
        balanceUnlimited: false,
        email: "",
        groupId: "",
        billingPlanId: "",
      });
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t("users.failedUpdate"));
    } finally {
      setSaving(false);
    }
  };

  const handleToggleEnabled = async (user: User) => {
    try {
      await updateUserOptimistic(
        user.id,
        { enabled: !user.enabled },
        users
      );
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t("users.failedUpdate"));
    }
  };

  const handleDelete = async (id: string) => {
    setDeleteTargetId(id);
  };

  const confirmDelete = async () => {
    if (!deleteTargetId) return;
    try {
      await deleteUserOptimistic(
        deleteTargetId,
        users
      );
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t("users.failedDelete"));
    } finally {
      setDeleteTargetId(null);
    }
  };

  const confirmAccountClassChange = async () => {
    if (!accountClassTarget || saving) return;
    setSaving(true);
    try {
      const updated = await updateUserAccountClassOptimistic(
        accountClassTarget.user.id,
        accountClassTarget.next,
        users,
      );
      setEditUser(updated);
      setFormData((current) => ({ ...current, groupId: updated.group_id }));
      setAccountClassTarget(null);
      toast.success(t("users.accountClassChanged"));
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t("users.failedUpdate"));
    } finally {
      setSaving(false);
    }
  };

  const openEdit = (user: User) => {
    setEditUser(user);
    setBalanceMode("set");
    setBalanceAddAmount("");
    setFormData({
      username: user.username,
      password: "",
      role: user.role,
      balanceUsd: user.balance_usd,
      balanceUnlimited: user.balance_unlimited,
      email: user.email ?? "",
      groupId: user.group_id,
      billingPlanId: user.billing_plan_id ?? "",
    });
  };

  const formatDate = (date: string) => {
    return new Date(date).toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  };

  const canEdit = (user: User) => {
    if (currentUser?.role === "super_admin") return true;
    if (user.role === "super_admin") return false;
    if (currentUser?.role === "admin") return true;
    return false;
  };

  const canDelete = (user: User) => {
    if (user.id === currentUser?.id) return false;
    if (user.role === "super_admin") return false;
    return canEdit(user);
  };

  if (isLoading) {
    return (
      <PageWrapper className="space-y-6">
        <TablePageSkeleton />
      </PageWrapper>
    );
  }

  return (
    <PageWrapper className="space-y-6">
      <motion.div
        initial={{ opacity: 0, y: -10 }}
        animate={{ opacity: 1, y: 0 }}
        transition={transitions.normal}
      >
        <PageHeader title={t("users.title")} description={t("users.description")} actions={(
          <Dialog open={createOpen} onOpenChange={setCreateOpen}>
          <DialogTrigger asChild>
            <AnimatedButton>
              <Button>
                <Plus className="mr-2 h-4 w-4" />
                {t("users.addUser")}
              </Button>
            </AnimatedButton>
          </DialogTrigger>
          <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-hidden p-0 sm:max-h-[calc(100dvh-3rem)]">
            <div className="flex min-h-0 flex-col p-6">
              <DialogHeader className="shrink-0">
                <DialogTitle>{t("users.createUser")}</DialogTitle>
                <DialogDescription>{t("users.addNewUser")}</DialogDescription>
              </DialogHeader>
              <div
                className="min-h-0 flex-1 overflow-y-auto pr-1"
                style={{ WebkitOverflowScrolling: "touch" }}
              >
                <div className="space-y-4 py-4">
                  <div className="space-y-2">
                    <Label htmlFor="username">{t("auth.username")}</Label>
                    <Input
                      id="username"
                      value={formData.username}
                      onChange={(e) => setFormData({ ...formData, username: e.target.value })}
                      placeholder="johndoe"
                      minLength={3}
                      maxLength={22}
                      pattern="[a-zA-Z0-9_]+"
                    />
                  </div>
                  <div className="space-y-2">
                    <Label htmlFor="password">{t("auth.password")}</Label>
                    <Input
                      id="password"
                      type="password"
                      value={formData.password}
                      onChange={(e) => setFormData({ ...formData, password: e.target.value })}
                      placeholder="••••••••"
                    />
                  </div>
                  <div className="space-y-2">
                    <Label>{t("users.role")}</Label>
                    <DropdownMenu>
                      <DropdownMenuTrigger asChild>
                        <Button variant="outline" className="w-full justify-start">
                          {t(`roles.${formData.role}`)}
                        </Button>
                      </DropdownMenuTrigger>
                      <DropdownMenuContent className="w-full">
                        {currentUser?.role === "super_admin" && (
                          <DropdownMenuItem onClick={() => setFormData({ ...formData, role: "admin" })}>
                            {t("roles.admin")}
                          </DropdownMenuItem>
                        )}
                        <DropdownMenuItem onClick={() => setFormData({ ...formData, role: "user" })}>
                          {t("roles.user")}
                        </DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                  </div>
                  <div className="space-y-2">
                    <Label htmlFor="user-group">{t("users.group")}</Label>
                    <GroupSingleSelect
                      id="user-group"
                      value={formData.groupId || defaultGroupId}
                      groups={groups}
                      loading={groupsLoading}
                      onChange={(groupId) => setFormData({ ...formData, groupId })}
                    />
                    <p className="text-xs text-muted-foreground">{t("users.groupHelp")}</p>
                  </div>
                </div>
              </div>
              <DialogFooter className="shrink-0 pt-4">
                <Button variant="outline" onClick={() => setCreateOpen(false)}>
                  {t("common.cancel")}
                </Button>
                <Button onClick={handleCreate} disabled={saving || !formData.username.trim() || !formData.password}>
                  {saving ? t("common.creating") : t("common.create")}
                </Button>
              </DialogFooter>
            </div>
          </DialogContent>
          </Dialog>
        )} />
      </motion.div>

      <AlertDialog open={!!deleteTargetId} onOpenChange={(open) => { if (!open) setDeleteTargetId(null); }}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("users.confirmDelete")}</AlertDialogTitle>
            <AlertDialogDescription>{t("users.confirmDelete")}</AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              onClick={confirmDelete}
            >
              {t("common.delete")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={accountClassTarget !== null}
        onOpenChange={(open) => { if (!open && !saving) setAccountClassTarget(null); }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("users.accountClassConfirmTitle")}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("users.accountClassDeleteKeysWarning", {
                username: accountClassTarget?.user.username,
                accountClass: accountClassTarget
                  ? t(`accountClass.${accountClassTarget.next}`)
                  : "",
              })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={saving}>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={saving}
              onClick={(event) => {
                event.preventDefault();
                void confirmAccountClassChange();
              }}
            >
              {saving ? t("common.saving") : t("users.accountClassConfirmAction")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <Dialog open={!!editUser} onOpenChange={(open) => !open && setEditUser(null)}>
        <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-hidden p-0 sm:max-h-[calc(100dvh-3rem)]">
          <div className="flex min-h-0 flex-col p-6">
            <DialogHeader className="shrink-0">
              <DialogTitle>{t("users.editUser")}</DialogTitle>
              <DialogDescription>{t("users.updateDetails")}</DialogDescription>
            </DialogHeader>
            <div
              className="min-h-0 flex-1 overflow-y-auto pr-1"
              style={{ WebkitOverflowScrolling: "touch" }}
            >
              <div className="space-y-4 py-4">
                <div className="space-y-2">
                  <Label htmlFor="edit-username">{t("auth.username")}</Label>
                  <Input
                    id="edit-username"
                    value={formData.username}
                    onChange={(e) => setFormData({ ...formData, username: e.target.value })}
                    minLength={3}
                    maxLength={22}
                    pattern="[a-zA-Z0-9_]+"
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="edit-password">{t("users.newPassword")}</Label>
                  <Input
                    id="edit-password"
                    type="password"
                    value={formData.password}
                    onChange={(e) => setFormData({ ...formData, password: e.target.value })}
                    placeholder="••••••••"
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="edit-email">{t("userSettings.email")}</Label>
                  <div className="relative">
                    <Mail className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
                    <Input
                      id="edit-email"
                      type="email"
                      value={formData.email}
                      onChange={(e) => setFormData({ ...formData, email: e.target.value })}
                      placeholder="user@example.com"
                      className="pl-9"
                    />
                  </div>
                  <p className="text-xs text-muted-foreground">{t("userSettings.emailDescription")}</p>
                </div>
                {currentUser?.role === "super_admin" && editUser?.role !== "super_admin" && (
                  <div className="space-y-2">
                    <Label>{t("users.role")}</Label>
                    <DropdownMenu>
                      <DropdownMenuTrigger asChild>
                        <Button variant="outline" className="w-full justify-start">
                          {t(`roles.${formData.role}`)}
                        </Button>
                      </DropdownMenuTrigger>
                      <DropdownMenuContent className="w-full">
                        <DropdownMenuItem onClick={() => setFormData({ ...formData, role: "admin" })}>
                          {t("roles.admin")}
                        </DropdownMenuItem>
                        <DropdownMenuItem onClick={() => setFormData({ ...formData, role: "user" })}>
                          {t("roles.user")}
                        </DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                  </div>
                )}
                {/* `editUser` must be narrowed here, not tested through `?.`. JSX props are
                    evaluated while this page renders, before the Dialog decides to mount, so
                    reading a field of a null `editUser` blanks the whole page. */}
                {editUser !== null && editUser.role !== "super_admin" && (
                  <div className="space-y-2 rounded-xl border bg-muted/20 p-4">
                    <div>
                      <Label>{t("users.accountClass")}</Label>
                      <p className="mt-1 text-xs text-muted-foreground">
                        {t("users.accountClassDescription")}
                      </p>
                    </div>
                    <Tabs value={editUser.account_class}>
                      <TabsList className="grid h-10 w-full grid-cols-4 rounded-lg">
                        {(["standard", "enterprise", "private", "agent"] as const).map((accountClass) => (
                          <TabsTrigger
                            key={accountClass}
                            value={accountClass}
                            onClick={() => {
                              if (accountClass !== editUser.account_class) {
                                setAccountClassTarget({ user: editUser, next: accountClass });
                              }
                            }}
                          >
                            {t(`accountClass.${accountClass}`)}
                          </TabsTrigger>
                        ))}
                      </TabsList>
                    </Tabs>
                  </div>
                )}
                <div className="space-y-2">
                  <Label htmlFor="edit-user-group">{t("users.group")}</Label>
                  <GroupSingleSelect
                    id="edit-user-group"
                    value={formData.groupId}
                    groups={groups.filter((group) => group.account_class === editUser?.account_class)}
                    loading={groupsLoading}
                    onChange={(groupId) => setFormData({ ...formData, groupId })}
                  />
                  <p className="text-xs text-muted-foreground">{t("users.groupHelp")}</p>
                </div>
                <div className="space-y-2">
                  <Label htmlFor="edit-billing-plan">{t("billingPlans.title")}</Label>
                  <Select
                    value={formData.billingPlanId || "none"}
                    onValueChange={(value) =>
                      setFormData({
                        ...formData,
                        billingPlanId: value === "none" ? "" : value,
                      })
                    }
                  >
                    <SelectTrigger id="edit-billing-plan" className="w-full">
                      <SelectValue placeholder={t("billingPlans.none")} />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="none">{t("billingPlans.none")}</SelectItem>
                      {billingPlans
                        .filter((p) => p.enabled || p.id === editUser?.billing_plan_id)
                        .map((plan) => (
                          <SelectItem key={plan.id} value={plan.id}>
                            {plan.name} · ${plan.grant_amount_usd}/
                            {plan.schedule}
                            {!plan.enabled ? ` (${t("common.disabled")})` : ""}
                          </SelectItem>
                        ))}
                    </SelectContent>
                  </Select>
                  {(() => {
                    const assigned = billingPlans.find((p) => p.id === editUser?.billing_plan_id);
                    if (!assigned || !editUser?.next_grant_at) return null;
                    return (
                      <p className="text-xs text-muted-foreground">
                        {t("billingPlans.nextReset", {
                          name: assigned.name,
                          time: new Date(editUser.next_grant_at).toLocaleString(),
                        })}
                      </p>
                    );
                  })()}
                </div>
                {currentUser?.role && (
                  <div className="space-y-2">
                    <div className="flex items-center justify-between gap-2">
                      <Label>{t("users.balance")}</Label>
                      <Tabs
                        value={balanceMode}
                        onValueChange={(v) => {
                          setBalanceMode(v as "set" | "add");
                          setBalanceAddAmount("");
                          setBalanceAddCurrency("CNY");
                        }}
                      >
                        <TabsList className="h-7">
                          <TabsTrigger value="set" className="px-2.5 py-0.5 text-xs">
                            {t("users.balanceSet")}
                          </TabsTrigger>
                          <TabsTrigger value="add" className="px-2.5 py-0.5 text-xs">
                            <PlusCircle className="mr-1 h-3 w-3" />
                            {t("users.balanceAdd")}
                          </TabsTrigger>
                        </TabsList>
                      </Tabs>
                    </div>
                    {balanceMode === "set" ? (
                      <Input
                        value={formData.balanceUsd}
                        onChange={(e) => setFormData({ ...formData, balanceUsd: e.target.value })}
                        placeholder="0"
                      />
                    ) : (
                      <>
                        <div className="flex items-center gap-2">
                          <div className="relative flex-1">
                            <span className="absolute left-3 top-1/2 -translate-y-1/2 text-sm text-muted-foreground">
                              {balanceAddCurrency === "CNY" ? "¥" : "$"}
                            </span>
                            <Input
                              className="pl-7"
                              value={balanceAddAmount}
                              onChange={(e) => setBalanceAddAmount(e.target.value)}
                              placeholder={t("users.balanceAddPlaceholder")}
                            />
                          </div>
                          <Tabs
                            value={balanceAddCurrency}
                            onValueChange={(v) => setBalanceAddCurrency(v as "USD" | "CNY")}
                          >
                            <TabsList className="h-9">
                              <TabsTrigger value="USD" className="px-2.5 py-0.5 text-xs">USD</TabsTrigger>
                              <TabsTrigger value="CNY" className="px-2.5 py-0.5 text-xs">CNY</TabsTrigger>
                            </TabsList>
                          </Tabs>
                        </div>
                        <p className="text-xs text-muted-foreground">
                          {t("users.balanceCurrentHint", { amount: editUser?.balance_usd ?? "0" })}
                          {(() => {
                            const addNano = amountToNanoForCurrency(
                              balanceAddAmount,
                              balanceAddCurrency,
                              exchangeRate?.cny_per_usd
                            );
                            if (!balanceAddAmount.trim() || addNano === null || !editUser) return null;
                            return (
                              <>
                                {" → "}
                                <span className="font-medium text-foreground">
                                  ${nanoToUsdString(BigInt(editUser.balance_nano_usd) + addNano)}
                                </span>
                                {balanceAddCurrency === "CNY" && exchangeRate?.cny_per_usd && (
                                  <span>
                                    {" "}
                                    ({t("users.balanceAddRate", { rate: exchangeRate.cny_per_usd })})
                                  </span>
                                )}
                              </>
                            );
                          })()}
                        </p>
                      </>
                    )}
                    <div className="flex items-center gap-2">
                      <Switch
                        checked={formData.balanceUnlimited}
                        onCheckedChange={(checked) =>
                          setFormData({ ...formData, balanceUnlimited: checked })
                        }
                      />
                      <span className="text-sm text-muted-foreground">{t("users.unlimited")}</span>
                    </div>
                  </div>
                )}
              </div>
            </div>
            <DialogFooter className="shrink-0 pt-4">
              <Button variant="outline" onClick={() => setEditUser(null)}>
                {t("common.cancel")}
              </Button>
              <Button onClick={handleUpdate} disabled={saving}>
                {saving ? t("common.saving") : t("common.save")}
              </Button>
            </DialogFooter>
          </div>
        </DialogContent>
      </Dialog>

      <motion.div
        initial={{ opacity: 0, y: 20 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ delay: 0.1, ...transitions.normal }}
      >
        <DataTableShell
          toolbar={(
            <div className="flex flex-col gap-3">
              <div className="-mx-1 overflow-x-auto px-1 pb-1">
                <Tabs value={scope} onValueChange={(value) => setScope(value as UserScope)}>
                  <TabsList className="grid h-10 w-full min-w-[32rem] grid-cols-5 rounded-lg sm:w-[40rem]">
                    {USER_SCOPES.map((value) => (
                      <TabsTrigger key={value} value={value} className="gap-1.5">
                        {t(`users.scopes.${value}`)}
                        <span className="text-xs text-muted-foreground tabular-nums">
                          {scopeCounts[value]}
                        </span>
                      </TabsTrigger>
                    ))}
                  </TabsList>
                </Tabs>
              </div>
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div>
                  <h2 className="text-base font-semibold">{t(`users.scopes.${scope}`)}</h2>
                  <p className="text-sm text-muted-foreground">
                    {t("users.usersTotal", { count: visibleUsers.length })}
                    {" · "}
                    {t("users.todaySummary", {
                      spend:
                        currency === "CNY" && exchangeRate?.cny_per_usd
                          ? formatCoinFromNanoUsdExact(
                              todayTotals.cost.toString(),
                              exchangeRate.cny_per_usd,
                              6,
                            )
                          : formatNanoUsd(todayTotals.cost, 2),
                      calls: todayTotals.calls.toLocaleString(),
                    })}
                  </p>
                </div>
                <div className="relative w-full sm:w-72">
                  <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    className="h-9 pl-10"
                    placeholder={t("users.searchPlaceholder")}
                    value={search}
                    onChange={(event) => setSearch(event.target.value)}
                  />
                </div>
              </div>
            </div>
          )}
          isEmpty={visibleUsers.length === 0}
          emptyState={(
            <EmptyState
              icon={<UserIcon className="h-12 w-12" />}
              title={t("users.allUsers")}
              description={t("users.noUsers")}
            />
          )}
        >
          {/* Mobile: one card per user. The eight-column table cannot fit a
              phone width without collapsing into itself. */}
          <div className="flex flex-col gap-2 lg:hidden">
            {visibleUsers.slice(0, 200).map((user) => {
              const RoleIcon = roleIcons[user.role];
              return (
                <div key={user.id} className="rounded-lg border bg-card p-3">
                  <div className="flex items-start gap-3">
                    <Avatar className="size-9 shrink-0">
                      {user.email && <AvatarImage src={getGravatarUrl(user.email, 64) ?? undefined} alt={user.username} />}
                      <AvatarFallback>{user.username[0].toUpperCase()}</AvatarFallback>
                    </Avatar>
                    <div className="min-w-0 flex-1">
                      <div className="flex min-w-0 flex-wrap items-center gap-1.5">
                        <span className="truncate font-medium">{user.username}</span>
                        <Badge variant={roleVariants[user.role]} className="h-6 gap-1 whitespace-nowrap px-1.5 text-xs">
                          <RoleIcon className="h-3 w-3" />
                          {t(`roles.${user.role}`)}
                        </Badge>
                        {!user.enabled && (
                          <Badge variant="outline" className="h-6 whitespace-nowrap px-1.5 text-xs text-muted-foreground">
                            {t("common.disabled")}
                          </Badge>
                        )}
                      </div>
                      <div className="mt-1 flex flex-wrap items-center gap-1.5">
                        {user.group_id && <GroupsBadge groupIds={[user.group_id]} />}
                        {user.billing_plan && (
                          <Badge variant={user.billing_plan.enabled ? "secondary" : "outline"} className="max-w-[10rem] truncate text-xs">
                            {user.billing_plan.name}
                          </Badge>
                        )}
                      </div>
                      <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-sm">
                        <div>
                          <dt className="text-xs text-muted-foreground">{t("users.balance")}</dt>
                          <dd className="tabular-nums">
                            {user.balance_unlimited
                              ? t("users.unlimited")
                              : currency === "CNY" && exchangeRate?.cny_per_usd
                                ? formatCoinFromNanoUsdForCurrency(user.balance_nano_usd, "CNY", exchangeRate.cny_per_usd)
                                : formatUsdDecimal(user.balance_usd, 2)}
                          </dd>
                        </div>
                        <div>
                          <dt className="text-xs text-muted-foreground">{t("users.todayUsage")}</dt>
                          <dd className="tabular-nums">
                            {currency === "CNY" && exchangeRate?.cny_per_usd
                              ? formatCoinFromNanoUsdExact(user.today_cost_nano_usd ?? "0", exchangeRate.cny_per_usd, 6)
                              : formatNanoUsd(user.today_cost_nano_usd, 2)}
                            <span className="ml-1 text-xs text-muted-foreground">
                              · {(user.today_calls ?? 0).toLocaleString()} {t("users.callsUnit")}
                            </span>
                          </dd>
                        </div>
                        <div className="col-span-2">
                          <dt className="text-xs text-muted-foreground">{t("users.createdLogin")}</dt>
                          <dd className="text-xs text-muted-foreground">
                            {formatDate(user.created_at)} · {user.last_login_at ? formatDate(user.last_login_at) : t("common.never")}
                          </dd>
                        </div>
                      </dl>
                    </div>
                  </div>
                  <div className="mt-2 flex items-center justify-between border-t pt-2">
                    <Switch
                      checked={user.enabled}
                      onCheckedChange={() => handleToggleEnabled(user)}
                      disabled={!canEdit(user)}
                      aria-label={t("common.status")}
                    />
                    <div className="flex items-center gap-1">
                      <Button
                        variant="ghost"
                        size="sm"
                        className="h-9 gap-1.5 px-2.5"
                        onClick={() => navigate(`/dashboard/logs?username=${encodeURIComponent(user.username)}`)}
                      >
                        <ScrollText className="h-4 w-4" />
                        {t("users.viewLogs")}
                      </Button>
                      {canEdit(user) && (
                        <Button variant="ghost" size="icon" className="size-9" aria-label={t("common.edit")} onClick={() => openEdit(user)}>
                          <Pencil className="h-4 w-4" />
                        </Button>
                      )}
                      {canDelete(user) && (
                        <Button
                          variant="ghost"
                          size="icon"
                          aria-label={t("common.delete")}
                          onClick={() => handleDelete(user.id)}
                          className="size-9 text-destructive hover:text-destructive"
                        >
                          <Trash2 className="h-4 w-4" />
                        </Button>
                      )}
                    </div>
                  </div>
                </div>
              );
            })}
          </div>

          {/* Desktop: six consolidated columns instead of the old eight. */}
          <div className="hidden lg:block">
            <TableVirtuoso
              style={{ height: "calc(100dvh - 300px)", minHeight: 400 }}
              data={visibleUsers}
              components={{
                Table: (props) => (
                  <table
                    {...props}
                    className="w-full table-fixed caption-bottom text-sm"
                  />
                ),
                TableHead: (props) => (
                  <thead {...props} className="[&_tr]:border-b" />
                ),
                TableRow: (props) => (
                  <tr
                    {...props}
                    className="border-b transition-colors hover:bg-muted/50"
                  />
                ),
                TableBody: (props) => (
                  <tbody {...props} className="[&_tr:last-child]:border-0" />
                ),
              }}
              fixedHeaderContent={() => (
                <tr className="border-b bg-background">
                  <VirtualTableHeaderCell>
                    {t("users.user")}
                  </VirtualTableHeaderCell>
                  <VirtualTableHeaderCell className="w-[7rem] whitespace-nowrap">
                    {t("users.role")}
                  </VirtualTableHeaderCell>
                  <VirtualTableHeaderCell className="w-[11rem]">
                    {t("users.balanceToday")}
                  </VirtualTableHeaderCell>
                  <VirtualTableHeaderCell className="w-[9.5rem]">
                    {t("users.createdLogin")}
                  </VirtualTableHeaderCell>
                  <VirtualTableHeaderCell className="w-[4rem]">
                    {t("common.status")}
                  </VirtualTableHeaderCell>
                  <VirtualTableHeaderCell className="w-[8rem]">
                    {t("common.actions")}
                  </VirtualTableHeaderCell>
                </tr>
              )}
              itemContent={(_index, user) => {
                const RoleIcon = roleIcons[user.role];
                return (
                  <>
                    <VirtualTableCell className="whitespace-nowrap">
                      <div className="flex min-w-0 items-center gap-2 py-1">
                        <Avatar className="size-8 shrink-0">
                          {user.email && <AvatarImage src={getGravatarUrl(user.email, 64) ?? undefined} alt={user.username} />}
                          <AvatarFallback>{user.username[0].toUpperCase()}</AvatarFallback>
                        </Avatar>
                        <div className="flex min-w-0 flex-wrap items-center gap-1.5">
                          <span className="min-w-0 truncate font-medium">{user.username}</span>
                          {user.group_id && (
                            <GroupsBadge groupIds={[user.group_id]} className="shrink-0 whitespace-nowrap" />
                          )}
                          {user.billing_plan && (
                            <Badge
                              variant={user.billing_plan.enabled ? "secondary" : "outline"}
                              className="max-w-[10rem] truncate text-xs"
                            >
                              {user.billing_plan.name}
                            </Badge>
                          )}
                        </div>
                      </div>
                    </VirtualTableCell>
                    <VirtualTableCell className="whitespace-nowrap">
                      <Badge variant={roleVariants[user.role]} className="h-7 gap-1 whitespace-nowrap">
                        <RoleIcon className="h-3 w-3 shrink-0" />
                        {t(`roles.${user.role}`)}
                      </Badge>
                    </VirtualTableCell>
                    <VirtualTableCell className="tabular-nums">
                      <div className="whitespace-nowrap">
                        <div>
                          {user.balance_unlimited
                            ? t("users.unlimited")
                            : currency === "CNY" && exchangeRate?.cny_per_usd
                              ? formatCoinFromNanoUsdForCurrency(
                                  user.balance_nano_usd,
                                  "CNY",
                                  exchangeRate.cny_per_usd,
                                )
                              : formatUsdDecimal(user.balance_usd, 2)}
                        </div>
                        <div className="text-xs text-muted-foreground">
                          {currency === "CNY" && exchangeRate?.cny_per_usd
                            ? formatCoinFromNanoUsdExact(
                                user.today_cost_nano_usd ?? "0",
                                exchangeRate.cny_per_usd,
                                6,
                              )
                            : formatNanoUsd(user.today_cost_nano_usd, 2)}
                          {" · "}
                          {(user.today_calls ?? 0).toLocaleString()} {t("users.callsUnit")}
                        </div>
                      </div>
                    </VirtualTableCell>
                    <VirtualTableCell className="tabular-nums">
                      <div className="whitespace-nowrap">
                        <div>{formatDate(user.created_at)}</div>
                        <div className="text-xs text-muted-foreground">
                          {user.last_login_at
                            ? formatDate(user.last_login_at)
                            : t("common.never")}
                        </div>
                      </div>
                    </VirtualTableCell>
                    <VirtualTableCell>
                      <Switch
                        checked={user.enabled}
                        onCheckedChange={() => handleToggleEnabled(user)}
                        disabled={!canEdit(user)}
                        aria-label={t("common.status")}
                      />
                    </VirtualTableCell>
                    <VirtualTableCell>
                      <div className="flex items-center gap-1">
                        <Button
                          variant="ghost"
                          size="icon"
                          className="size-9"
                          title={t("users.viewLogs")}
                          aria-label={t("users.viewLogs")}
                          onClick={() =>
                            navigate(`/dashboard/logs?username=${encodeURIComponent(user.username)}`)
                          }
                        >
                          <ScrollText className="h-4 w-4" />
                        </Button>
                        {canEdit(user) && (
                          <Button
                            variant="ghost"
                            size="icon"
                            className="size-9"
                            aria-label={t("common.edit")}
                            onClick={() => openEdit(user)}
                          >
                            <Pencil className="h-4 w-4" />
                          </Button>
                        )}
                        {canDelete(user) && (
                          <Button
                            variant="ghost"
                            size="icon"
                            aria-label={t("common.delete")}
                            onClick={() => handleDelete(user.id)}
                            className="size-9 text-destructive hover:text-destructive"
                          >
                            <Trash2 className="h-4 w-4" />
                          </Button>
                        )}
                      </div>
                    </VirtualTableCell>
                  </>
                );
              }}
            />
          </div>
        </DataTableShell>

      </motion.div>
    </PageWrapper>
  );
}
