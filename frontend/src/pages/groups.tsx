import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { ArrowDown, ArrowUp, Boxes, GripVertical, Pencil, Plus, Trash2, UserRoundPlus, UserX, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
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
import { toast } from "sonner";
import { PageWrapper, motion, transitions } from "@/components/ui/motion";
import { PageHeader } from "@/components/ui/page-header";
import { TablePageSkeleton } from "@/components/ui/page-skeleton";
import { EmptyState } from "@/components/ui/empty-state";
import {
  useDashboardGroups,
  createGroupOptimistic,
  updateGroupOptimistic,
  reorderGroupsOptimistic,
  deleteGroupOptimistic,
} from "@/lib/swr";
import { api } from "@/lib/api";
import type { AccountClass, Group, User } from "@/lib/api";
import { cn } from "@/lib/utils";

interface GroupFormState {
  name: string;
  description: string;
  user_selectable: boolean;
  sort_order: string;
  confirm_public_exposure: boolean;
}

const EMPTY_FORM: GroupFormState = {
  name: "",
  description: "",
  user_selectable: true,
  sort_order: "0",
  confirm_public_exposure: false,
};

function formFromGroup(group: Group): GroupFormState {
  return {
    name: group.name,
    description: group.description,
    user_selectable: group.user_selectable,
    sort_order: String(group.sort_order),
    confirm_public_exposure: false,
  };
}

function useFinePointer() {
  const [isFinePointer, setIsFinePointer] = useState(() =>
    typeof window === "undefined" ? false : window.matchMedia("(pointer: fine)").matches
  );

  useEffect(() => {
    const media = window.matchMedia("(pointer: fine)");
    const syncPointerState = () => setIsFinePointer(media.matches);
    syncPointerState();
    media.addEventListener("change", syncPointerState);
    return () => media.removeEventListener("change", syncPointerState);
  }, []);

  return isFinePointer;
}

const ACCOUNT_CLASSES = ["standard", "enterprise", "private", "agent"] as const;

export function GroupsPage() {
  const { t } = useTranslation();
  const { data, isLoading, error, mutate: reloadGroups } = useDashboardGroups();
  const groups = useMemo(() => data ?? [], [data]);
  const [accountClass, setAccountClass] = useState<AccountClass>("standard");
  const visibleGroups = useMemo(
    () => groups.filter((group) => group.account_class === accountClass),
    [accountClass, groups],
  );

  const [createOpen, setCreateOpen] = useState(false);
  const [form, setForm] = useState<GroupFormState>(EMPTY_FORM);
  const [editTarget, setEditTarget] = useState<Group | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<Group | null>(null);
  const [grantsTarget, setGrantsTarget] = useState<Group | null>(null);
  const [grants, setGrants] = useState<{ user_id: string; username: string }[]>([]);
  const [grantsLoading, setGrantsLoading] = useState(false);
  const [grantUserId, setGrantUserId] = useState("");
  const [users, setUsers] = useState<User[]>([]);
  const [saving, setSaving] = useState(false);
  const [reordering, setReordering] = useState(false);
  const [draggingGroupId, setDraggingGroupId] = useState<string | null>(null);
  const canDrag = useFinePointer();

  const openCreate = () => {
    setForm({ ...EMPTY_FORM, sort_order: String(visibleGroups.length) });
    setCreateOpen(true);
  };

  const openGrants = async (group: Group) => {
    setGrantsTarget(group);
    setGrantsLoading(true);
    setGrantUserId("");
    try {
      const [grantsResult, usersResult] = await Promise.all([
        api.listGroupGrants(group.id),
        api.listUsers(),
      ]);
      setGrants(grantsResult.users);
      setUsers(usersResult);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t("groups.grants.loadFailed"));
    } finally {
      setGrantsLoading(false);
    }
  };

  const grantAccess = async () => {
    if (!grantsTarget || !grantUserId) return;
    try {
      await api.grantGroupAccess(grantUserId, grantsTarget.id);
      const result = await api.listGroupGrants(grantsTarget.id);
      setGrants(result.users);
      setGrantUserId("");
      toast.success(t("groups.grants.granted"));
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t("groups.grants.loadFailed"));
    }
  };

  const revokeAccess = async (userId: string) => {
    if (!grantsTarget) return;
    try {
      await api.revokeGroupAccess(userId, grantsTarget.id);
      setGrants((current) => current.filter((grant) => grant.user_id !== userId));
      toast.success(t("groups.grants.revoked"));
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t("groups.grants.loadFailed"));
    }
  };

  const validateForm = (): { name: string; description: string; sort_order: number; confirm_public_exposure: boolean } | null => {
    const name = form.name.trim();
    if (!name || name.length > 64) {
      toast.error(t("groups.invalidName"));
      return null;
    }
    const description = form.description.trim();
    if (description.length > 256) {
      toast.error(t("groups.invalidDescription"));
      return null;
    }
    const sortOrder = Number(form.sort_order.trim() || "0");
    if (!Number.isInteger(sortOrder)) {
      toast.error(t("groups.invalidSortOrder"));
      return null;
    }
    const nameChanged = !editTarget || name.normalize("NFC") !== editTarget.name.trim().normalize("NFC");
    if (nameChanged && !form.confirm_public_exposure) {
      toast.error(t("groups.publicExposureRequired"));
      return null;
    }
    return { name, description, sort_order: sortOrder, confirm_public_exposure: form.confirm_public_exposure };
  };

  const handleCreate = async () => {
    const validated = validateForm();
    if (!validated || saving) return;
    setSaving(true);
    try {
      await createGroupOptimistic(
        { ...validated, is_public: form.user_selectable, account_class: accountClass },
        groups,
        (error) => toast.error(error.message)
      );
      setCreateOpen(false);
      toast.success(t("common.success"));
    } catch {
      // optimistic helper already rolled back and toasted
    } finally {
      setSaving(false);
    }
  };

  const handleUpdate = async () => {
    if (!editTarget) return;
    const validated = validateForm();
    if (!validated || saving) return;
    setSaving(true);
    try {
      await updateGroupOptimistic(
        editTarget.id,
        { ...validated, is_public: form.user_selectable },
        groups,
        (error) => toast.error(error.message)
      );
      setEditTarget(null);
      toast.success(t("common.success"));
    } catch {
      // optimistic helper already rolled back and toasted
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!deleteTarget) return;
    try {
      await deleteGroupOptimistic(deleteTarget.id, groups, (error) =>
        toast.error(error.message)
      );
      toast.success(t("common.success"));
    } catch {
      // optimistic helper already rolled back and toasted
    } finally {
      setDeleteTarget(null);
    }
  };

  const toggleUserSelectable = async (group: Group, userSelectable: boolean) => {
    await updateGroupOptimistic(
      group.id,
      { is_public: userSelectable },
      groups,
      (error) => toast.error(error.message)
    ).catch(() => undefined);
  };

  const applyReorder = async (orderedGroups: Group[]) => {
    if (reordering) return;
    setReordering(true);
    try {
      await reorderGroupsOptimistic(
        orderedGroups.map((group) => group.id),
        groups,
        (error) => toast.error(error.message)
      );
      toast.success(t("groups.reorderSuccess"));
    } catch {
      // optimistic helper already rolled back and toasted
    } finally {
      setDraggingGroupId(null);
      setReordering(false);
    }
  };

  const moveGroup = async (from: number, to: number) => {
    if (to < 0 || to >= visibleGroups.length || from === to || reordering) return;
    const next = [...visibleGroups];
    const [group] = next.splice(from, 1);
    next.splice(to, 0, group);
    await applyReorder(next);
  };

  const handleDrop = async (targetGroupId: string) => {
    if (!draggingGroupId || draggingGroupId === targetGroupId || reordering) {
      setDraggingGroupId(null);
      return;
    }
    const next = [...visibleGroups];
    const from = next.findIndex((group) => group.id === draggingGroupId);
    const to = next.findIndex((group) => group.id === targetGroupId);
    if (from < 0 || to < 0) {
      setDraggingGroupId(null);
      return;
    }
    const [group] = next.splice(from, 1);
    next.splice(to, 0, group);
    await applyReorder(next);
  };

  if (error) {
    return (
      <PageWrapper className="space-y-6">
        <PageHeader title={t("groups.title")} />
        <EmptyState variant="inline" role="alert" title={t("common.error")}
          action={<Button variant="outline" onClick={() => void reloadGroups()}>
            <RefreshCw className="mr-2 h-4 w-4" />{t("common.retry")}
          </Button>} />
      </PageWrapper>
    );
  }

  if (isLoading) {
    return <PageWrapper className="space-y-6"><TablePageSkeleton /></PageWrapper>;
  }

  const renderForm = (onSubmit: () => void) => (
    <>
      <div className="grid gap-4 py-4">
        <div className="grid gap-2">
          <Label htmlFor="group-name">{t("groups.name")}</Label>
          <Input
            id="group-name"
            value={form.name}
            maxLength={64}
            onChange={(e) => setForm({ ...form, name: e.target.value })}
            placeholder={t("groups.namePlaceholder")}
          />
        </div>
        <div className="grid gap-2">
          <Label htmlFor="group-description">{t("groups.descriptionLabel")}</Label>
          <Textarea
            id="group-description"
            value={form.description}
            maxLength={256}
            rows={2}
            onChange={(e) => setForm({ ...form, description: e.target.value })}
            placeholder={t("groups.descriptionPlaceholder")}
          />
          <p className="text-xs text-muted-foreground">{t("groups.descriptionHelp")}</p>
        </div>
        <div className="flex items-start gap-3 rounded-md border p-3">
          <Checkbox id="group-public-exposure" checked={form.confirm_public_exposure} onCheckedChange={(checked) => setForm({ ...form, confirm_public_exposure: checked === true })} />
          <Label htmlFor="group-public-exposure" className="text-sm font-normal leading-5">{t("groups.publicExposureConfirm")}</Label>
        </div>
        <div className="grid gap-2">
          <Label htmlFor="group-sort-order">{t("groups.sortOrder")}</Label>
          <Input
            id="group-sort-order"
            inputMode="numeric"
            value={form.sort_order}
            onChange={(e) => setForm({ ...form, sort_order: e.target.value })}
            placeholder="0"
          />
          <p className="text-xs text-muted-foreground">{t("groups.sortOrderHelp")}</p>
        </div>
        <div className="flex items-center justify-between rounded-lg border p-3">
          <div>
            <Label htmlFor="group-user-selectable">{t("groups.userSelectable")}</Label>
            <p className="mt-1 text-xs text-muted-foreground">
              {t("groups.userSelectableHelp")}
            </p>
          </div>
          <Switch
            id="group-user-selectable"
            checked={form.user_selectable}
            onCheckedChange={(checked) => setForm({ ...form, user_selectable: checked })}
          />
        </div>
      </div>
      <DialogFooter>
        <Button type="button" onClick={onSubmit} disabled={saving}>
          {saving ? t("common.loading") : t("common.save")}
        </Button>
      </DialogFooter>
    </>
  );

  return (
    <PageWrapper>
      <motion.div
        initial={{ opacity: 0, y: 12 }}
        animate={{ opacity: 1, y: 0 }}
        transition={transitions.normal}
        className="space-y-6"
      >
        <PageHeader
          title={t("groups.title")}
          description={t("groups.description")}
          actions={
            <Button onClick={openCreate}>
              <Plus className="mr-2 h-4 w-4" />
              {t("groups.create")}
            </Button>
          }
        />

        <div className="flex flex-col gap-2 rounded-xl border bg-card p-3 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <p className="text-sm font-medium">{t("groups.accountClass")}</p>
            <p className="text-xs text-muted-foreground">{t("groups.accountClassDescription")}</p>
          </div>
          <Tabs value={accountClass} onValueChange={(value) => setAccountClass(value as AccountClass)}>
            <TabsList className="grid w-full grid-cols-4 rounded-lg sm:w-[32rem]">
              {ACCOUNT_CLASSES.map((value) => (
                <TabsTrigger key={value} value={value}>{t(`accountClass.${value}`)}</TabsTrigger>
              ))}
            </TabsList>
          </Tabs>
        </div>

        {isLoading ? (
          <TablePageSkeleton />
        ) : visibleGroups.length === 0 ? (
          <EmptyState
            variant="card"
            icon={<Boxes className="h-10 w-10 text-muted-foreground" />}
            title={t("groups.emptyTitle")}
            description={t("groups.emptyDescription")}
          />
        ) : (
          <div className="overflow-x-auto rounded-lg border">
            <table className="w-full min-w-[48rem] text-sm">
              <thead className="bg-muted/50 text-left text-muted-foreground">
                <tr>
                  <th className="px-4 py-2.5 font-medium">{t("groups.name")}</th>
                  <th className="px-4 py-2.5 font-medium">{t("groups.descriptionLabel")}</th>
                  <th className="px-4 py-2.5 font-medium">{t("groups.userSelectable")}</th>
                  <th className="px-4 py-2.5 font-medium">{t("groups.sortOrder")}</th>
                  <th className="px-4 py-2.5" />
                </tr>
              </thead>
              <tbody>
                {visibleGroups.map((group, index) => (
                  <tr
                    key={group.id}
                    className={cn(
                      "border-t transition-colors hover:bg-accent/40",
                      draggingGroupId === group.id && "bg-accent/50 opacity-60"
                    )}
                    onDragOver={(event) => {
                      if (canDrag && !reordering) event.preventDefault();
                    }}
                    onDrop={(event) => {
                      event.preventDefault();
                      if (canDrag) void handleDrop(group.id);
                    }}
                  >
                    <td className="px-4 py-3">
                      <div className="flex items-center gap-2">
                        <span className="font-medium">{group.name}</span>
                      </div>
                    </td>
                    <td className="max-w-[20rem] px-4 py-3">
                      <span className="block truncate text-muted-foreground">
                        {group.description || "—"}
                      </span>
                    </td>
                    <td className="px-4 py-3">
                      <Switch
                        checked={group.is_public}
                        onCheckedChange={(checked) => toggleUserSelectable(group, checked)}
                      />
                    </td>
                    <td className="px-4 py-3">
                      <div className="flex items-center gap-1">
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon"
                          className="hidden size-8 cursor-grab touch-manipulation active:cursor-grabbing [@media(pointer:fine)]:inline-flex"
                          draggable={canDrag && !reordering}
                          disabled={!canDrag || reordering}
                          aria-label={t("groups.dragToReorder")}
                          title={t("groups.dragToReorder")}
                          onDragStart={(event) => {
                            event.dataTransfer.effectAllowed = "move";
                            event.dataTransfer.setData("text/plain", group.id);
                            setDraggingGroupId(group.id);
                          }}
                          onDragEnd={() => setDraggingGroupId(null)}
                        >
                          <GripVertical className="h-4 w-4" />
                        </Button>
                        <span className="min-w-6 text-center tabular-nums">
                          {group.sort_order}
                        </span>
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon"
                          className="size-11 touch-manipulation sm:size-8"
                          disabled={index === 0 || reordering}
                          aria-label={t("groups.moveUp")}
                          title={t("groups.moveUp")}
                          onClick={() => void moveGroup(index, index - 1)}
                        >
                          <ArrowUp className="h-4 w-4" />
                        </Button>
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon"
                          className="size-11 touch-manipulation sm:size-8"
                          disabled={index === visibleGroups.length - 1 || reordering}
                          aria-label={t("groups.moveDown")}
                          title={t("groups.moveDown")}
                          onClick={() => void moveGroup(index, index + 1)}
                        >
                          <ArrowDown className="h-4 w-4" />
                        </Button>
                      </div>
                    </td>
                    <td className="px-4 py-3">
                      <div className="flex items-center justify-end gap-1">
                        <Button
                          variant="ghost"
                          size="icon"
                          className="size-11 touch-manipulation sm:size-9"
                          aria-label={t("common.edit")}
                          onClick={() => {
                            setForm(formFromGroup(group));
                            setEditTarget(group);
                          }}
                        >
                          <Pencil className="h-4 w-4" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon"
                          className="size-11 touch-manipulation sm:size-9"
                          aria-label={t("groups.grants.title")}
                          onClick={() => void openGrants(group)}
                        >
                          <UserRoundPlus className="h-4 w-4" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon"
                          className="size-11 touch-manipulation sm:size-9"
                          aria-label={t("common.delete")}
                          onClick={() => setDeleteTarget(group)}
                        >
                          <Trash2 className="h-4 w-4 text-destructive" />
                        </Button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        {/* Create dialog */}
        <Dialog open={createOpen} onOpenChange={setCreateOpen}>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>{t("groups.create")}</DialogTitle>
              <DialogDescription>{t("groups.createDescription")}</DialogDescription>
            </DialogHeader>
            {renderForm(handleCreate)}
          </DialogContent>
        </Dialog>

        {/* Edit dialog */}
        <Dialog open={editTarget !== null} onOpenChange={(open) => !open && setEditTarget(null)}>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>{t("groups.edit")}</DialogTitle>
              <DialogDescription>
                {editTarget?.is_default
                  ? t("groups.editDefaultDescription")
                  : editTarget?.name}
              </DialogDescription>
            </DialogHeader>
            {renderForm(handleUpdate)}
          </DialogContent>
        </Dialog>

        {/* Delete confirm */}
        <AlertDialog
          open={deleteTarget !== null}
          onOpenChange={(open) => !open && setDeleteTarget(null)}
        >
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>{t("groups.deleteTitle")}</AlertDialogTitle>
              <AlertDialogDescription>
                {t("groups.deleteDescription", { name: deleteTarget?.name })}
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
              <AlertDialogAction
                className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
                onClick={handleDelete}
              >
                {t("common.delete")}
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>

        {/* Per-user grants for private Group access (groups-registry.spec.md GR-U). */}
        <Dialog open={grantsTarget !== null} onOpenChange={(open) => !open && setGrantsTarget(null)}>
          <DialogContent className="sm:max-w-md">
            <DialogHeader>
              <DialogTitle>{t("groups.grants.title", { name: grantsTarget?.name })}</DialogTitle>
              <DialogDescription>{t("groups.grants.description")}</DialogDescription>
            </DialogHeader>
            <div className="flex items-center gap-2">
              <Select value={grantUserId} onValueChange={setGrantUserId}>
                <SelectTrigger className="flex-1">
                  <SelectValue placeholder={t("groups.grants.pickUser")} />
                </SelectTrigger>
                <SelectContent>
                  {users
                    .filter((user) => !grants.some((grant) => grant.user_id === user.id))
                    .map((user) => (
                      <SelectItem key={user.id} value={user.id}>
                        {user.username}
                      </SelectItem>
                    ))}
                </SelectContent>
              </Select>
              <Button onClick={() => void grantAccess()} disabled={!grantUserId}>
                <Plus className="mr-1 h-4 w-4" />
                {t("groups.grants.add")}
              </Button>
            </div>
            {grantsLoading ? (
              <p className="py-4 text-center text-sm text-muted-foreground">
                {t("common.loading")}
              </p>
            ) : grants.length === 0 ? (
              <p className="py-4 text-center text-sm text-muted-foreground">
                {t("groups.grants.empty")}
              </p>
            ) : (
              <ul className="flex max-h-64 flex-col gap-1 overflow-auto">
                {grants.map((grant) => (
                  <li
                    key={grant.user_id}
                    className="flex items-center justify-between rounded-md border px-3 py-2 text-sm"
                  >
                    <span className="truncate">{grant.username}</span>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="size-8"
                      aria-label={t("groups.grants.revoke")}
                      onClick={() => void revokeAccess(grant.user_id)}
                    >
                      <UserX className="h-4 w-4 text-destructive" />
                    </Button>
                  </li>
                ))}
              </ul>
            )}
          </DialogContent>
        </Dialog>
      </motion.div>
    </PageWrapper>
  );
}
