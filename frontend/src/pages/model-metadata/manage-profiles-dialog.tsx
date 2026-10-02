import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Copy, Trash2 } from "lucide-react";
import { toast } from "sonner";

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
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import type { BillingRateProfileSummary } from "@/lib/api";
import { copyPricingProfile, deletePricingProfileOptimistic } from "@/lib/swr";

/**
 * UI25: profile lifecycle in one dialog — copy (MB-A7) and delete (MB-A11).
 * A profile still routing traffic (match rules / providers) refuses deletion
 * with the server's inline message instead of disappearing.
 */
export function ManageProfilesDialog({
  open,
  onOpenChange,
  profiles,
  onProfileDeleted,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  profiles: BillingRateProfileSummary[];
  onProfileDeleted: (deleted: string) => void;
}) {
  const { t } = useTranslation();
  const [copyTarget, setCopyTarget] = useState<string | null>(null);
  const [copyName, setCopyName] = useState("");
  const [copying, setCopying] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<BillingRateProfileSummary | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  const runCopy = async () => {
    if (!copyTarget) return;
    setCopying(true);
    try {
      await copyPricingProfile(copyTarget, copyName.trim());
      toast.success(
        t("modelMetadata.profiles.copied", { target: copyName.trim(), source: copyTarget })
      );
      setCopyTarget(null);
      setCopyName("");
    } catch (error) {
      toast.error(
        error instanceof Error ? error.message : t("modelMetadata.profiles.copyFailed")
      );
    } finally {
      setCopying(false);
    }
  };

  const runDelete = async () => {
    if (!deleteTarget) return;
    setDeleting(true);
    setDeleteError(null);
    try {
      const result = await deletePricingProfileOptimistic(
        deleteTarget.pricing_profile,
        profiles
      );
      toast.success(
        t("modelMetadata.profiles.deleted", {
          profile: deleteTarget.pricing_profile,
          rates: result.deleted_rates,
        })
      );
      onProfileDeleted(deleteTarget.pricing_profile);
      setDeleteTarget(null);
    } catch (error) {
      // A 409 keeps the row listed and shows the server's reason inline.
      setDeleteError(
        error instanceof Error ? error.message : t("modelMetadata.profiles.deleteFailed")
      );
    } finally {
      setDeleting(false);
    }
  };

  return (
    <>
      <Dialog open={open} onOpenChange={onOpenChange}>
        <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-hidden rounded-2xl p-0 sm:max-w-lg">
          <div className="flex max-h-[calc(100dvh-2rem)] flex-col p-5 sm:p-6">
            <DialogHeader className="shrink-0 pr-10">
              <DialogTitle>{t("modelMetadata.profiles.manageTitle")}</DialogTitle>
              <DialogDescription className="mt-2 text-pretty">
                {t("modelMetadata.profiles.manageDescription")}
              </DialogDescription>
            </DialogHeader>

            <div className="mt-4 min-h-0 flex-1 overflow-y-auto pr-1">
              {profiles.map((profile) => (
                <div
                  key={profile.pricing_profile}
                  className="flex items-center justify-between gap-2 border-b py-2.5 last:border-b-0"
                >
                  <div className="min-w-0">
                    <p className="truncate font-mono text-sm font-medium">
                      {profile.pricing_profile}
                    </p>
                    <p className="text-xs text-muted-foreground">
                      {t("modelMetadata.profiles.modelCount", { count: profile.model_count })}
                      {profile.rate_count > 0 &&
                        ` · ${t("modelMetadata.profiles.rateCount", { count: profile.rate_count })}`}
                    </p>
                  </div>
                  <div className="flex shrink-0 items-center gap-1">
                    {profile.has_models_dev && (
                      <Badge variant="secondary" className="mr-1 font-mono text-[10px]">
                        models.dev
                      </Badge>
                    )}
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => {
                        setCopyTarget(profile.pricing_profile);
                        setCopyName("");
                      }}
                      aria-label={t("modelMetadata.profiles.copy")}
                    >
                      <Copy data-icon />
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => {
                        setDeleteTarget(profile);
                        setDeleteError(null);
                      }}
                      aria-label={t("modelMetadata.profiles.delete")}
                    >
                      <Trash2 data-icon className="text-destructive" />
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          </div>
        </DialogContent>
      </Dialog>

      <Dialog open={copyTarget !== null} onOpenChange={(next) => { if (!next) setCopyTarget(null); }}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>{t("modelMetadata.profiles.copyTitle")}</DialogTitle>
            <DialogDescription>{copyTarget ?? ""}</DialogDescription>
          </DialogHeader>
          <div className="flex flex-col gap-2 py-2">
            <Label htmlFor="copy-profile-name">{t("modelMetadata.profiles.newName")}</Label>
            <Input
              id="copy-profile-name"
              value={copyName}
              onChange={(event) => setCopyName(event.target.value)}
              className="font-mono"
            />
            <p className="text-xs text-muted-foreground">
              {t("modelMetadata.profiles.copyHint")}
            </p>
          </div>
          <div className="flex justify-end gap-2">
            <Button variant="outline" onClick={() => setCopyTarget(null)}>
              {t("common.cancel")}
            </Button>
            <Button
              disabled={copying || !copyName.trim() || copyName.trim() === copyTarget}
              onClick={() => void runCopy()}
            >
              {copying ? t("common.saving") : t("modelMetadata.profiles.copyConfirm")}
            </Button>
          </div>
        </DialogContent>
      </Dialog>

      <AlertDialog
        open={deleteTarget !== null}
        onOpenChange={(next) => {
          if (!next) {
            setDeleteTarget(null);
            setDeleteError(null);
          }
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {t("modelMetadata.profiles.deleteTitle", {
                profile: deleteTarget?.pricing_profile ?? "",
              })}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {t("modelMetadata.profiles.deleteConfirm", {
                rates: deleteTarget?.rate_count ?? 0,
                models: deleteTarget?.model_count ?? 0,
              })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          {deleteError && (
            <p className="text-sm text-destructive">{deleteError}</p>
          )}
          <AlertDialogFooter>
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={deleting}
              onClick={(event) => {
                event.preventDefault();
                void runDelete();
              }}
            >
              {deleting ? t("common.saving") : t("common.delete")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}
