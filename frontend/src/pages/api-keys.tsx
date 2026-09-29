import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Plus, Trash2, Copy, Check, Key, Edit, Globe, Layers, Settings2, ArrowRightLeft, X, BarChart3 } from "lucide-react";
import { ApiKeyAnalyticsDialog } from "@/components/api-key-analytics-dialog";
import { BadgeOverflowList } from "@/components/BadgeOverflowList";
import { GroupsBadge } from "@/components/GroupsBadge";
import { GroupMultiSelect } from "@/components/groups/GroupPicker";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { Checkbox } from "@/components/ui/checkbox";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
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
  useApiKeys,
  createApiKeyOptimistic,
  updateApiKeyOptimistic,
  deleteApiKeyOptimistic,
  batchDeleteApiKeysOptimistic,
  useDashboardGroups,
  useApiKeyChannelConflicts,
  useApiKeyModelConflicts,
  useTransformRegistry,
} from "@/lib/swr";
import type { ApiKey, ApiKeyChannelBinding, ApiKeyChannelConflict, ApiKeyCreated, ApiKeyModelBinding, ApiKeyModelConflict, CreateApiKeyInput, Group, ModelRedirectRule, RequestCaptureMode, TransformRuleConfig, UpdateApiKeyInput } from "@/lib/api";
import { AnimatedButton, PageWrapper, motion, transitions } from "@/components/ui/motion";
import { PageHeader } from "@/components/ui/page-header";
import { TablePageSkeleton } from "@/components/ui/page-skeleton";
import { Skeleton } from "@/components/ui/skeleton";
import { DataTableShell, VirtualTableCell, VirtualTableHeaderCell } from "@/components/ui/data-table-shell";
import { EmptyState } from "@/components/ui/empty-state";
import { TransformChainEditor } from "@/components/transforms/transform-chain-editor";
import { findFirstInvalidTransformRule } from "@/components/transforms/transform-schema";
import { toast } from "sonner";
import { useAuth } from "@/hooks/use-auth";
import { normalizeMultiplier } from "@/lib/exact-decimal";
import { buildSpendLimitPayloadIn, nanoToLimitInput, nanoToUsdInput, type SpendLimitDraft } from "@/lib/spend-limits";
import { SpendLimitsEditor } from "@/components/SpendLimitsEditor";
import { useStoreExchangeRate } from "@/hooks/use-store-exchange-rate";

function parseOptionalMultiplier(value: string): string | undefined {
  if (!value.trim()) return undefined;
  const normalized = normalizeMultiplier(value);
  if (normalized == null) {
    throw new Error("Multiplier must be a positive decimal with at most 9 fractional digits");
  }
  return normalized;
}


function requestCaptureBadgeVariant(mode: RequestCaptureMode): "secondary" | "outline" {
  return mode === "capture-only-abnormal" ? "secondary" : "outline";
}

function ApiKeyRestrictionBadges({
  apiKey,
  t,
}: {
  apiKey: ApiKey;
  t: (key: string) => string;
}) {
  const captureLabel =
    apiKey.request_capture_mode === "capture-only-abnormal"
      ? t("apiKeys.captureBadgeAbnormal")
      : t("apiKeys.captureBadgeAll");
  const captureHelp =
    apiKey.request_capture_mode === "capture-only-abnormal"
      ? t("apiKeys.requestCaptureModeAbnormalHelp")
      : t("apiKeys.requestCaptureModeAllHelp");
  const items = [
    ...(apiKey.model_limits_enabled && apiKey.model_limits.length > 0
      ? [
          {
            key: "model-limits",
            collapsed: (
              <Badge variant="outline" className="gap-1 px-1.5 text-xs">
                <Layers className="h-3 w-3 shrink-0" />
                {apiKey.model_limits.length}
              </Badge>
            ),
            full: (
              <Badge variant="outline" className="max-w-none gap-1 px-1.5 text-xs">
                <Layers className="h-3 w-3 shrink-0" />
                <span className="whitespace-nowrap">
                  {t("apiKeys.modelLimits")}: {apiKey.model_limits.join(", ")}
                </span>
              </Badge>
            ),
          },
        ]
      : []),
    ...(apiKey.ip_whitelist.length > 0
      ? [
          {
            key: "ip-whitelist",
            collapsed: (
              <Badge variant="outline" className="gap-1 px-1.5 text-xs">
                <Globe className="h-3 w-3 shrink-0" />
                {apiKey.ip_whitelist.length}
              </Badge>
            ),
            full: (
              <Badge variant="outline" className="max-w-none gap-1 px-1.5 text-xs">
                <Globe className="h-3 w-3 shrink-0" />
                <span className="whitespace-nowrap">
                  {t("apiKeys.ipWhitelist")}: {apiKey.ip_whitelist.join(", ")}
                </span>
              </Badge>
            ),
          },
        ]
      : []),
    ...(apiKey.max_multiplier != null
      ? [
          {
            key: "max-multiplier",
            collapsed: (
              <Badge variant="outline" className="px-1.5 text-xs">
                ≤{apiKey.max_multiplier}x
              </Badge>
            ),
            full: (
              <Badge variant="outline" className="max-w-none px-1.5 text-xs">
                <span className="whitespace-nowrap">
                  {t("apiKeys.maxMultiplier")}: {apiKey.max_multiplier}x
                </span>
              </Badge>
            ),
          },
        ]
      : []),
    ...(apiKey.request_capture_mode !== "off"
      ? [
          {
            key: "request-capture",
            collapsed: (
              <Badge
                variant={requestCaptureBadgeVariant(apiKey.request_capture_mode)}
                className="px-1.5 text-xs"
              >
                {captureLabel}
              </Badge>
            ),
            full: (
              <Badge
                variant={requestCaptureBadgeVariant(apiKey.request_capture_mode)}
                className="max-w-none px-1.5 text-xs"
              >
                <span className="whitespace-nowrap">
                  {captureLabel}: {captureHelp}
                </span>
              </Badge>
            ),
          },
        ]
      : []),
  ];

  if (items.length === 0) {
    return <span className="text-muted-foreground">-</span>;
  }

  return (
    <BadgeOverflowList
      items={items}
      visibleCount={2}
      popoverOnSingle
      ariaLabel={t("apiKeys.restrictions")}
      className="max-w-[24rem]"
      contentClassName="max-w-[min(34rem,calc(100vw-2rem))]"
    />
  );
}

interface KeyGroupsSectionProps {
  idPrefix: string;
  groupIds: string[];
  groups: Group[];
  groupsLoading: boolean;
  ownerGroupId: string | null;
  isAdmin: boolean;
  onGroupIdsChange: (next: string[]) => void;
}

function scopedChannelConflicts(
  conflicts: ApiKeyChannelConflict[],
  groupIds: string[],
  modelLimitsEnabled: boolean,
  modelLimits: string[],
) {
  return conflicts.filter((conflict) => (
    (groupIds.length === 0 || groupIds.includes(conflict.group_id))
    && (!modelLimitsEnabled || modelLimits.length === 0 || modelLimits.includes(conflict.model))
  ));
}

function scopedChannelBindings(
  conflicts: ApiKeyChannelConflict[],
  bindings: ApiKeyChannelBinding[],
) {
  return conflicts.flatMap((conflict) => {
    const binding = bindings.find((candidate) => (
      candidate.group_id === conflict.group_id && candidate.model === conflict.model
    ));
    return binding && conflict.options.some((option) => option.channel_id === binding.channel_id)
      ? [binding]
      : [];
  });
}

function unresolvedChannelConflicts(
  conflicts: ApiKeyChannelConflict[],
  bindings: ApiKeyChannelBinding[],
) {
  return conflicts.filter((conflict) => !bindings.some((binding) => (
    binding.group_id === conflict.group_id
    && binding.model === conflict.model
    && conflict.options.some((option) => option.channel_id === binding.channel_id)
  )));
}

/**
 * TM-GRP-1..TM-GRP-5: an empty selection permits every Group. Non-admins may
 * only pick `user_selectable` Groups plus their own current Group.
 */
function KeyGroupsSection({
  idPrefix,
  groupIds,
  groups,
  groupsLoading,
  ownerGroupId,
  isAdmin,
  onGroupIdsChange,
}: KeyGroupsSectionProps) {
  const { t } = useTranslation();

  return (
    <div className="space-y-3 rounded-lg border p-3">
      <Label htmlFor={`${idPrefix}-groups`}>{t("apiKeys.groups")}</Label>
      <GroupMultiSelect
        value={groupIds}
        groups={groups}
        loading={groupsLoading}
        sortable
        optionFilter={
          isAdmin
            ? undefined
            : (group) => group.user_selectable || group.id === ownerGroupId
        }
        onChange={onGroupIdsChange}
      />
      <p className="text-xs text-muted-foreground">{t("apiKeys.groupsHelp")}</p>
    </div>
  );
}

function scopedModelConflicts(
  conflicts: ApiKeyModelConflict[],
  groupIds: string[],
  modelLimitsEnabled: boolean,
  modelLimits: string[],
) {
  return conflicts.filter((conflict) => {
    if (modelLimitsEnabled && modelLimits.length > 0 && !modelLimits.includes(conflict.model)) {
      return false;
    }
    const options = conflict.options.filter((option) => (
      groupIds.length === 0 || groupIds.includes(option.group_id)
    ));
    return options.length > 1;
  }).map((conflict) => ({
    ...conflict,
    options: conflict.options.filter((option) => (
      groupIds.length === 0 || groupIds.includes(option.group_id)
    )),
  }));
}

function scopedModelBindings(
  conflicts: ApiKeyModelConflict[],
  bindings: ApiKeyModelBinding[],
) {
  return conflicts.flatMap((conflict) => {
    const binding = bindings.find((candidate) => candidate.model === conflict.model);
    return binding && conflict.options.some((option) => option.group_id === binding.group_id)
      ? [binding]
      : [];
  });
}

function unresolvedModelConflicts(
  conflicts: ApiKeyModelConflict[],
  bindings: ApiKeyModelBinding[],
) {
  return conflicts.filter((conflict) => !bindings.some((binding) => (
    binding.model === conflict.model
    && conflict.options.some((option) => option.group_id === binding.group_id)
  )));
}

function ModelBindingsSection({
  conflicts,
  loading,
  groupIds,
  modelLimitsEnabled,
  modelLimits,
  value,
  onChange,
  failed,
}: {
  conflicts: ApiKeyModelConflict[];
  loading: boolean;
  groupIds: string[];
  modelLimitsEnabled: boolean;
  modelLimits: string[];
  value: ApiKeyModelBinding[];
  onChange: (next: ApiKeyModelBinding[]) => void;
  failed: boolean;
}) {
  const { t } = useTranslation();
  const scoped = scopedModelConflicts(conflicts, groupIds, modelLimitsEnabled, modelLimits);
  if (loading) return <Skeleton className="h-24 w-full rounded-lg" />;
  if (failed) return <p role="alert" className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">{t("apiKeys.modelSelectionLoadError")}</p>;
  if (scoped.length === 0) return null;
  return (
    <div className="space-y-3 rounded-lg border p-3">
      <div>
        <p className="text-sm font-medium">{t("apiKeys.modelSelection")}</p>
        <p className="mt-1 text-xs text-muted-foreground">{t("apiKeys.modelSelectionHelp")}</p>
      </div>
      {scoped.map((conflict) => {
        const selected = value.find((binding) => binding.model === conflict.model);
        return (
          <div key={conflict.model} className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_minmax(220px,0.8fr)] sm:items-center">
            <div className="min-w-0">
              <p className="truncate text-sm font-medium">{conflict.model}</p>
              <p className="truncate text-xs text-muted-foreground">{t("apiKeys.modelSelectionConflictHint")}</p>
            </div>
            <Select
              value={selected?.group_id ?? ""}
              onValueChange={(groupId) => onChange([
                ...value.filter((binding) => binding.model !== conflict.model),
                { model: conflict.model, group_id: groupId },
              ])}
            >
              <SelectTrigger><SelectValue placeholder={t("apiKeys.selectModelGroup")} /></SelectTrigger>
              <SelectContent>
                {conflict.options.map((option) => (
                  <SelectItem key={option.group_id} value={option.group_id}>
                    {option.group_name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        );
      })}
      {unresolvedModelConflicts(scoped, value).length > 0 ? (
        <p role="alert" className="text-xs text-destructive">
          {t("apiKeys.modelSelectionRequired", { count: unresolvedModelConflicts(scoped, value).length })}
        </p>
      ) : null}
    </div>
  );
}

function ChannelBindingsSection({
  conflicts,
  loading,
  groupIds,
  modelLimitsEnabled,
  modelLimits,
  value,
  onChange,
  failed,
}: {
  conflicts: ApiKeyChannelConflict[];
  loading: boolean;
  groupIds: string[];
  modelLimitsEnabled: boolean;
  modelLimits: string[];
  value: ApiKeyChannelBinding[];
  onChange: (next: ApiKeyChannelBinding[]) => void;
  failed: boolean;
}) {
  const { t } = useTranslation();
  const scoped = scopedChannelConflicts(conflicts, groupIds, modelLimitsEnabled, modelLimits);
  if (loading) return <Skeleton className="h-24 w-full rounded-lg" />;
  if (failed) return <p role="alert" className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">{t("apiKeys.channelSelectionLoadError")}</p>;
  if (scoped.length === 0) return null;
  return (
    <div className="space-y-3 rounded-lg border p-3">
      <div>
        <p className="text-sm font-medium">{t("apiKeys.channelSelection")}</p>
        <p className="mt-1 text-xs text-muted-foreground">{t("apiKeys.channelSelectionHelp")}</p>
      </div>
      {scoped.map((conflict) => {
        const selected = value.find((binding) => (
          binding.group_id === conflict.group_id && binding.model === conflict.model
        ));
        return (
          <div key={`${conflict.group_id}:${conflict.model}`} className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_minmax(220px,0.8fr)] sm:items-center">
            <div className="min-w-0">
              <p className="truncate text-sm font-medium">{conflict.model}</p>
              <p className="truncate text-xs text-muted-foreground">{conflict.group_name}</p>
            </div>
            <Select
              value={selected?.channel_id ?? ""}
              onValueChange={(channelId) => onChange([
                ...value.filter((binding) => !(
                  binding.group_id === conflict.group_id && binding.model === conflict.model
                )),
                { group_id: conflict.group_id, model: conflict.model, channel_id: channelId },
              ])}
            >
              <SelectTrigger><SelectValue placeholder={t("apiKeys.selectChannel")} /></SelectTrigger>
              <SelectContent>
                {conflict.options.map((option) => (
                  <SelectItem key={option.channel_id} value={option.channel_id}>
                    {option.provider_name} · {option.channel_name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        );
      })}
      {unresolvedChannelConflicts(scoped, value).length > 0 ? (
        <p role="alert" className="text-xs text-destructive">
          {t("apiKeys.channelSelectionRequired", { count: unresolvedChannelConflicts(scoped, value).length })}
        </p>
      ) : null}
    </div>
  );
}

interface ModelRedirectsEditorProps {
  value: ModelRedirectRule[];
  onChange: (next: ModelRedirectRule[]) => void;
}

function ModelRedirectsEditor({ value, onChange }: ModelRedirectsEditorProps) {
  const { t } = useTranslation();

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <ArrowRightLeft className="h-4 w-4 text-muted-foreground" />
          <h3 className="text-sm font-medium">{t("apiKeys.modelRedirects")}</h3>
        </div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => onChange([...value, { pattern: "", replace: "" }])}
        >
          <Plus className="mr-1 h-3 w-3" />
          {t("common.add")}
        </Button>
      </div>
      {value.length === 0 && (
        <p className="text-sm text-muted-foreground">{t("apiKeys.modelRedirectsEmpty")}</p>
      )}
      {value.map((rule, idx) => (
        <div key={idx} className="flex items-center gap-2">
          <Input
            value={rule.pattern}
            onChange={(e) => {
              const updated = [...value];
              updated[idx] = { ...updated[idx], pattern: e.target.value };
              onChange(updated);
            }}
            placeholder=".*opus.*"
            className="flex-1 font-mono text-sm"
          />
          <span className="shrink-0 text-sm text-muted-foreground">→</span>
          <Input
            value={rule.replace}
            onChange={(e) => {
              const updated = [...value];
              updated[idx] = { ...updated[idx], replace: e.target.value };
              onChange(updated);
            }}
            placeholder="gpt-5.4"
            className="flex-1 font-mono text-sm"
          />
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className="size-11 touch-manipulation sm:size-8 shrink-0"
            aria-label={t("common.delete")}
            onClick={() => onChange(value.filter((_, i) => i !== idx))}
          >
            <X className="h-4 w-4" />
          </Button>
        </div>
      ))}
      <p className="text-xs text-muted-foreground">{t("apiKeys.modelRedirectsHelp")}</p>
    </div>
  );
}

export function ApiKeysPage() {
  const { t } = useTranslation();
  const { user: currentUser } = useAuth();
  const { data: keys = [], isLoading } = useApiKeys();
  const { data: groups = [], isLoading: groupsLoading } = useDashboardGroups();
  const { data: channelConflicts = [], isLoading: channelConflictsLoading, error: channelConflictsError } =
    useApiKeyChannelConflicts();
  const { data: modelConflicts = [], isLoading: modelConflictsLoading, error: modelConflictsError } =
    useApiKeyModelConflicts();
  const canManageSystem = currentUser?.role === "admin" || currentUser?.role === "super_admin";
  const { data: transformRegistry = [], isLoading: transformRegistryLoading } =
    useTransformRegistry({ isPaused: () => !canManageSystem });
  const apiKeyTransformRegistry = useMemo(
    () => transformRegistry.filter((item) => item.supported_scopes.includes("api_key")),
    [transformRegistry]
  );
  const [createOpen, setCreateOpen] = useState(false);
  const [editKey, setEditKey] = useState<ApiKey | null>(null);
  const [selectedKeys, setSelectedKeys] = useState<string[]>([]);
  const [deleteTargetId, setDeleteTargetId] = useState<string | null>(null);
  const [batchDeleteOpen, setBatchDeleteOpen] = useState(false);
  const [analyticsKey, setAnalyticsKey] = useState<ApiKey | null>(null);

  // Create form state
  const [newKeyName, setNewKeyName] = useState("");
  const [newKeyExpires, setNewKeyExpires] = useState("");
  /// ORGL-18: per-key spend limits typed as USD; empty = unlimited. The same
  /// three windows as the org-space Limits view.
  const [newKeySpendLimits, setNewKeySpendLimits] = useState<SpendLimitDraft>({ total: "", hourly: "", daily: "" });
  const [newKeyLimitCurrency, setNewKeyLimitCurrency] = useState<"USD" | "CNY">("CNY");
  const { data: exchangeRate } = useStoreExchangeRate();
  const [newKeyModelLimitsEnabled, setNewKeyModelLimitsEnabled] = useState(false);
  const [newKeyModelLimits, setNewKeyModelLimits] = useState("");
  const [newKeyIpWhitelist, setNewKeyIpWhitelist] = useState("");

  const [newKeyGroupIds, setNewKeyGroupIds] = useState<string[]>([]);
  const [newKeyChannelBindings, setNewKeyChannelBindings] = useState<ApiKeyChannelBinding[]>([]);
  const [newKeyModelBindings, setNewKeyModelBindings] = useState<ApiKeyModelBinding[]>([]);
  const [newKeyMaxMultiplier, setNewKeyMaxMultiplier] = useState("");
  const [newKeyTransforms, setNewKeyTransforms] = useState<TransformRuleConfig[]>([]);
  const [newKeyModelRedirects, setNewKeyModelRedirects] = useState<ModelRedirectRule[]>([]);
  const [newKeyReasoningEnvelopeEnabled, setNewKeyReasoningEnvelopeEnabled] = useState(true);
  const [newKeyRequestCaptureMode, setNewKeyRequestCaptureMode] = useState<RequestCaptureMode>("off");

  const [creating, setCreating] = useState(false);
  const [updating, setUpdating] = useState(false);
  const [createdKey, setCreatedKey] = useState<ApiKeyCreated | null>(null);
  const [copiedKey, setCopiedKey] = useState<string | null>(null);
  const newKeyModelList = useMemo(
    () => newKeyModelLimits.split(",").map((model) => model.trim()).filter(Boolean),
    [newKeyModelLimits],
  );
  const activeChannelConflicts = useMemo(
    () => scopedChannelConflicts(
      channelConflicts,
      newKeyGroupIds,
      newKeyModelLimitsEnabled,
      newKeyModelList,
    ),
    [channelConflicts, newKeyGroupIds, newKeyModelLimitsEnabled, newKeyModelList],
  );
  const activeChannelBindings = useMemo(
    () => scopedChannelBindings(activeChannelConflicts, newKeyChannelBindings),
    [activeChannelConflicts, newKeyChannelBindings],
  );
  const activeModelConflicts = useMemo(
    () => scopedModelConflicts(
      modelConflicts,
      newKeyGroupIds,
      newKeyModelLimitsEnabled,
      newKeyModelList,
    ),
    [modelConflicts, newKeyGroupIds, newKeyModelLimitsEnabled, newKeyModelList],
  );
  const activeModelBindings = useMemo(
    () => scopedModelBindings(activeModelConflicts, newKeyModelBindings),
    [activeModelConflicts, newKeyModelBindings],
  );
  const channelSelectionBlocked = channelConflictsLoading
    || Boolean(channelConflictsError)
    || unresolvedChannelConflicts(activeChannelConflicts, activeChannelBindings).length > 0;
  const modelSelectionBlocked = modelConflictsLoading
    || Boolean(modelConflictsError)
    || unresolvedModelConflicts(activeModelConflicts, activeModelBindings).length > 0;

  const resetCreateForm = () => {
    setNewKeyName("");
    setNewKeyExpires("");
    setNewKeySpendLimits({ total: "", hourly: "", daily: "" });
    setNewKeyModelLimitsEnabled(false);
    setNewKeyModelLimits("");
    setNewKeyIpWhitelist("");

    setNewKeyGroupIds([]);
    setNewKeyChannelBindings([]);
    setNewKeyModelBindings([]);
    setNewKeyMaxMultiplier("");
    setNewKeyTransforms([]);
    setNewKeyModelRedirects([]);
    setNewKeyReasoningEnvelopeEnabled(true);
    setNewKeyRequestCaptureMode("off");
  };

  const handleCreate = async () => {
    if (!newKeyName.trim()) return;
    if (modelSelectionBlocked) {
      toast.error(t("apiKeys.modelSelectionRequired", {
        count: unresolvedModelConflicts(activeModelConflicts, activeModelBindings).length,
      }));
      return;
    }
    if (channelSelectionBlocked) {
      toast.error(t("apiKeys.channelSelectionRequired", {
        count: unresolvedChannelConflicts(activeChannelConflicts, activeChannelBindings).length,
      }));
      return;
    }
    const invalidRule = findFirstInvalidTransformRule(newKeyTransforms, apiKeyTransformRegistry);
    if (invalidRule) {
      const firstError = invalidRule.errors[0];
      toast.error(t("transforms.validationRuleInvalid", {
        index: invalidRule.index + 1,
        reason: `${firstError.field} ${firstError.message}`,
      }));
      return;
    }
    setCreating(true);
    try {
      const input: CreateApiKeyInput = {
        name: newKeyName.trim(),
        expires_in_days: newKeyExpires ? parseInt(newKeyExpires) : undefined,
        ...buildSpendLimitPayloadIn(
          newKeySpendLimits,
          newKeyLimitCurrency,
          exchangeRate?.cny_per_usd,
          false,
        ),
        model_limits_enabled: newKeyModelLimitsEnabled,
        model_limits: newKeyModelList,
        ip_whitelist: newKeyIpWhitelist ? newKeyIpWhitelist.split(",").map(s => s.trim()).filter(s => s) : [],
        group_ids: newKeyGroupIds,
        channel_bindings: activeChannelBindings,
        model_bindings: activeModelBindings,
        max_multiplier: parseOptionalMultiplier(newKeyMaxMultiplier),
        transforms: newKeyTransforms,
        model_redirects: newKeyModelRedirects.filter((r) => r.pattern.trim() && r.replace.trim()),
        reasoning_envelope_enabled: newKeyReasoningEnvelopeEnabled,
        request_capture_mode: newKeyRequestCaptureMode,
      };
      const key = await createApiKeyOptimistic(
        input,
        keys
      );
      setCreatedKey(key);
      resetCreateForm();
      setCreateOpen(false);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t("apiKeys.failedCreate"));
    } finally {
      setCreating(false);
    }
  };

  const handleUpdate = async () => {
    if (!editKey) return;
    if (modelSelectionBlocked) {
      toast.error(t("apiKeys.modelSelectionRequired", {
        count: unresolvedModelConflicts(activeModelConflicts, activeModelBindings).length,
      }));
      return;
    }
    if (channelSelectionBlocked) {
      toast.error(t("apiKeys.channelSelectionRequired", {
        count: unresolvedChannelConflicts(activeChannelConflicts, activeChannelBindings).length,
      }));
      return;
    }
    const invalidRule = findFirstInvalidTransformRule(newKeyTransforms, apiKeyTransformRegistry);
    if (invalidRule) {
      const firstError = invalidRule.errors[0];
      toast.error(t("transforms.validationRuleInvalid", {
        index: invalidRule.index + 1,
        reason: `${firstError.field} ${firstError.message}`,
      }));
      return;
    }
    setUpdating(true);
    try {
      const input: UpdateApiKeyInput = {
        name: newKeyName.trim() || undefined,
        ...buildSpendLimitPayloadIn(
          newKeySpendLimits,
          newKeyLimitCurrency,
          exchangeRate?.cny_per_usd,
          true,
        ),
        model_limits_enabled: newKeyModelLimitsEnabled,
        model_limits: newKeyModelList,
        ip_whitelist: newKeyIpWhitelist ? newKeyIpWhitelist.split(",").map(s => s.trim()).filter(s => s) : [],
        group_ids: newKeyGroupIds,
        channel_bindings: activeChannelBindings,
        model_bindings: activeModelBindings,
        max_multiplier: parseOptionalMultiplier(newKeyMaxMultiplier),
        transforms: newKeyTransforms,
        model_redirects: newKeyModelRedirects.filter((r) => r.pattern.trim() && r.replace.trim()),
        reasoning_envelope_enabled: newKeyReasoningEnvelopeEnabled,
        request_capture_mode: newKeyRequestCaptureMode,
      };
      await updateApiKeyOptimistic(
        editKey.id,
        input,
        keys
      );
      setEditKey(null);
      resetCreateForm();
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t("apiKeys.failedUpdate"));
    } finally {
      setUpdating(false);
    }
  };

  const handleDelete = async (id: string) => {
    setDeleteTargetId(id);
  };

  const confirmDelete = async () => {
    if (!deleteTargetId) return;
    try {
      await deleteApiKeyOptimistic(
        deleteTargetId,
        keys
      );
      setSelectedKeys(prev => prev.filter(k => k !== deleteTargetId));
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t("apiKeys.failedDelete"));
    } finally {
      setDeleteTargetId(null);
    }
  };

  const handleBatchDelete = async () => {
    if (selectedKeys.length === 0) return;
    setBatchDeleteOpen(true);
  };

  const confirmBatchDelete = async () => {
    try {
      await batchDeleteApiKeysOptimistic(
        selectedKeys,
        keys
      );
      setSelectedKeys([]);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t("apiKeys.failedBatchDelete"));
    } finally {
      setBatchDeleteOpen(false);
    }
  };

  const handleToggleEnabled = async (key: ApiKey) => {
    try {
      await updateApiKeyOptimistic(
        key.id,
        { enabled: !key.enabled },
        keys
      );
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t("apiKeys.failedUpdate"));
    }
  };

  const handleCopy = async (key: string) => {
    await navigator.clipboard.writeText(key);
    setCopiedKey(key);
    setTimeout(() => setCopiedKey(null), 2000);
  };

  const openEditDialog = (key: ApiKey) => {
    setEditKey(key);
    setNewKeyName(key.name);
    // Stored limits are nano-USD; prefill in CNY only when the fixed rate is loaded,
    // otherwise an unconvertible CNY prefill would render the limits as empty.
    const editCurrency = exchangeRate?.cny_per_usd ? "CNY" : "USD";
    setNewKeySpendLimits({
      total: nanoToLimitInput(key.spend_limit_total_nano_usd, editCurrency, exchangeRate?.cny_per_usd),
      hourly: nanoToLimitInput(key.spend_limit_hourly_nano_usd, editCurrency, exchangeRate?.cny_per_usd),
      daily: nanoToLimitInput(key.spend_limit_daily_nano_usd, editCurrency, exchangeRate?.cny_per_usd),
    });
    setNewKeyLimitCurrency(editCurrency);
    setNewKeyModelLimitsEnabled(key.model_limits_enabled);
    setNewKeyModelLimits(key.model_limits.join(", "));
    setNewKeyIpWhitelist(key.ip_whitelist.join(", "));
    setNewKeyGroupIds(key.group_ids ?? []);
    setNewKeyChannelBindings(key.channel_bindings ?? []);
    setNewKeyModelBindings(key.model_bindings ?? []);
    setNewKeyMaxMultiplier(key.max_multiplier != null ? String(key.max_multiplier) : "");
    setNewKeyTransforms(key.transforms ?? []);
    setNewKeyModelRedirects(key.model_redirects ?? []);
    setNewKeyReasoningEnvelopeEnabled(key.reasoning_envelope_enabled ?? true);
    setNewKeyRequestCaptureMode(key.request_capture_mode ?? "off");
  };

  const toggleSelectKey = (id: string) => {
    setSelectedKeys(prev =>
      prev.includes(id) ? prev.filter(k => k !== id) : [...prev, id]
    );
  };

  const toggleSelectAll = () => {
    if (selectedKeys.length === keys.length) {
      setSelectedKeys([]);
    } else {
      setSelectedKeys(keys.map(k => k.id));
    }
  };

  const formatDate = (date: string) => {
    return new Date(date).toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
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
        <PageHeader title={t("apiKeys.title")} description={t("apiKeys.description")} actions={(
          <>
          {selectedKeys.length > 0 && (
            <Button variant="destructive" onClick={handleBatchDelete}>
              <Trash2 className="mr-2 h-4 w-4" />
              {t("apiKeys.batchDelete")}
            </Button>
          )}
          <Dialog open={createOpen} onOpenChange={setCreateOpen}>
            <DialogTrigger asChild>
              <AnimatedButton>
                <Button>
                  <Plus className="mr-2 h-4 w-4" />
                  {t("apiKeys.createKey")}
                </Button>
              </AnimatedButton>
            </DialogTrigger>
            <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-hidden p-0 sm:max-h-[calc(100dvh-3rem)] sm:max-w-4xl">
              <div className="flex min-h-0 flex-col p-6">
              <DialogHeader className="shrink-0">
                <DialogTitle>{t("apiKeys.createApiKey")}</DialogTitle>
                <DialogDescription>
                  {t("apiKeys.createDescription")}
                </DialogDescription>
              </DialogHeader>
              <div className="min-h-0 flex-1 space-y-4 overflow-y-auto px-1 py-4 -mx-1">
                <div className="space-y-2">
                  <Label htmlFor="name">{t("common.name")}</Label>
                  <Input
                    id="name"
                    value={newKeyName}
                    onChange={(e) => setNewKeyName(e.target.value)}
                    placeholder="My API Key"
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="expires">{t("apiKeys.expiresInDays")}</Label>
                  <Input
                    id="expires"
                    type="number"
                    min="1"
                    value={newKeyExpires}
                    onChange={(e) => setNewKeyExpires(e.target.value)}
                    placeholder="30"
                  />
                </div>
                <KeyGroupsSection
                  idPrefix="create"
                  groupIds={newKeyGroupIds}
                  groups={groups}
                  groupsLoading={groupsLoading}
                  ownerGroupId={currentUser?.group_id ?? null}
                  isAdmin={canManageSystem}
                  onGroupIdsChange={setNewKeyGroupIds}
                />
                <ModelBindingsSection
                  conflicts={modelConflicts}
                  loading={modelConflictsLoading}
                  groupIds={newKeyGroupIds}
                  modelLimitsEnabled={newKeyModelLimitsEnabled}
                  modelLimits={newKeyModelLimits.split(",").map((model) => model.trim()).filter(Boolean)}
                  value={newKeyModelBindings}
                  onChange={setNewKeyModelBindings}
                  failed={Boolean(modelConflictsError)}
                />
                <ChannelBindingsSection
                  conflicts={channelConflicts}
                  loading={channelConflictsLoading}
                  groupIds={newKeyGroupIds}
                  modelLimitsEnabled={newKeyModelLimitsEnabled}
                  modelLimits={newKeyModelLimits.split(",").map((model) => model.trim()).filter(Boolean)}
                  value={newKeyChannelBindings}
                  onChange={setNewKeyChannelBindings}
                  failed={Boolean(channelConflictsError)}
                />
                <SpendLimitsEditor
                  idPrefix="create"
                  draft={newKeySpendLimits}
                  currency={newKeyLimitCurrency}
                  onCurrencyChange={setNewKeyLimitCurrency}
                  onChange={setNewKeySpendLimits}
                  cnyPerUsd={exchangeRate?.cny_per_usd}
                />
                <div className="space-y-1">
                  <div className="flex items-center space-x-2">
                    <Switch
                      id="reasoningEnvelopeEnabled"
                      checked={newKeyReasoningEnvelopeEnabled}
                      onCheckedChange={setNewKeyReasoningEnvelopeEnabled}
                    />
                    <Label htmlFor="reasoningEnvelopeEnabled">{t("apiKeys.reasoningEnvelopeEnabled")}</Label>
                  </div>
                  <p className="text-sm text-muted-foreground">{t("apiKeys.reasoningEnvelopeHelp")}</p>
                </div>
                <div className="space-y-1">
                  <div className="flex items-center space-x-2">
                    <Label htmlFor="requestCaptureMode">{t("apiKeys.requestCaptureMode")}</Label>
                  </div>
                  <Select value={newKeyRequestCaptureMode} onValueChange={(value) => setNewKeyRequestCaptureMode(value as RequestCaptureMode)}>
                    <SelectTrigger id="requestCaptureMode">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="off">{t("apiKeys.requestCaptureModeOff")}</SelectItem>
                      <SelectItem value="capture-all">{t("apiKeys.requestCaptureModeAll")}</SelectItem>
                      <SelectItem value="capture-only-abnormal">{t("apiKeys.requestCaptureModeAbnormal")}</SelectItem>
                    </SelectContent>
                  </Select>
                  <p className="text-sm text-muted-foreground">{t("apiKeys.requestCaptureHelp")}</p>
                </div>
                <div className="flex items-center space-x-2">
                  <Switch
                    id="modelLimitsEnabled"
                    checked={newKeyModelLimitsEnabled}
                    onCheckedChange={setNewKeyModelLimitsEnabled}
                  />
                  <Label htmlFor="modelLimitsEnabled">{t("apiKeys.enableModelLimits")}</Label>
                </div>
                {newKeyModelLimitsEnabled && (
                  <div className="space-y-2">
                    <Label htmlFor="modelLimits">{t("apiKeys.allowedModels")}</Label>
                    <Input
                      id="modelLimits"
                      value={newKeyModelLimits}
                      onChange={(e) => setNewKeyModelLimits(e.target.value)}
                      placeholder="gpt-4, gpt-3.5-turbo"
                    />
                    <p className="text-sm text-muted-foreground">{t("apiKeys.modelsHelp")}</p>
                  </div>
                )}
                <div className="space-y-2">
                  <Label htmlFor="ipWhitelist">{t("apiKeys.ipWhitelist")}</Label>
                  <Input
                    id="ipWhitelist"
                    value={newKeyIpWhitelist}
                    onChange={(e) => setNewKeyIpWhitelist(e.target.value)}
                    placeholder="192.168.1.1, 10.0.0.0/8"
                  />
                  <p className="text-sm text-muted-foreground">{t("apiKeys.ipHelp")}</p>
                </div>
                <div className="space-y-2">
                  <Label htmlFor="maxMultiplier">{t("apiKeys.maxMultiplier")}</Label>
                  <Input
                    id="maxMultiplier"
                    type="text"
                    inputMode="decimal"
                    value={newKeyMaxMultiplier}
                    onChange={(e) => setNewKeyMaxMultiplier(e.target.value)}
                    placeholder="e.g. 1.5"
                  />
                  <p className="text-sm text-muted-foreground">{t("apiKeys.maxMultiplierHelp")}</p>
                </div>
                <div className="space-y-3">
                  <div className="flex items-center gap-2">
                    <Settings2 className="h-4 w-4 text-muted-foreground" />
                    <h3 className="text-sm font-medium">{t("transforms.titleApiKey")}</h3>
                  </div>
                  <TransformChainEditor
                    value={newKeyTransforms}
                    registry={apiKeyTransformRegistry}
                    loading={transformRegistryLoading}
                    onChange={setNewKeyTransforms}
                  />
                </div>
                <ModelRedirectsEditor
                  value={newKeyModelRedirects}
                  onChange={setNewKeyModelRedirects}
                />
              </div>
              <DialogFooter className="shrink-0 pt-4">
                <Button variant="outline" onClick={() => { setCreateOpen(false); resetCreateForm(); }}>
                  {t("common.cancel")}
                </Button>
                <Button onClick={handleCreate} disabled={creating || !newKeyName.trim() || channelSelectionBlocked}>
                  {creating ? t("common.creating") : t("common.create")}
                </Button>
              </DialogFooter>
              </div>
            </DialogContent>
          </Dialog>
          </>
        )} />
      </motion.div>

      {createdKey && (
        <motion.div
          initial={{ opacity: 0, y: 20, scale: 0.95 }}
          animate={{ opacity: 1, y: 0, scale: 1 }}
          transition={{ type: "spring", stiffness: 300, damping: 25 }}
        >
          <Card className="border-success-border bg-success-soft">
            <CardHeader>
              <CardTitle className="flex items-center gap-2 text-success-foreground">
                <motion.div
                  initial={{ scale: 0 }}
                  animate={{ scale: 1 }}
                  transition={{ delay: 0.2, type: "spring", stiffness: 300 }}
                >
                  <Key className="h-5 w-5" />
                </motion.div>
                {t("apiKeys.apiKeyCreated")}
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="flex items-center gap-2">
                <code className="flex-1 rounded-lg border bg-muted px-3 py-2 text-sm">
                  {createdKey.key}
                </code>
                <AnimatedButton>
                  <Button
                    variant="outline"
                    size="icon"
                    className="size-11 touch-manipulation sm:size-9"
                    aria-label={t("common.copy")}
                    onClick={() => handleCopy(createdKey.key)}
                  >
                    {copiedKey === createdKey.key ? (
                      <Check className="h-4 w-4" />
                    ) : (
                      <Copy className="h-4 w-4" />
                    )}
                  </Button>
                </AnimatedButton>
              </div>
              <Button
                variant="ghost"
                size="sm"
                className="mt-2"
                onClick={() => setCreatedKey(null)}
              >
                {t("common.dismiss")}
              </Button>
            </CardContent>
          </Card>
        </motion.div>
      )}

      <motion.div
        initial={{ opacity: 0, y: 20 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ delay: 0.1, ...transitions.normal }}
      >
        <DataTableShell
          toolbar={(
            <div>
              <h2 className="text-base font-semibold">{t("apiKeys.yourApiKeys")}</h2>
              <p className="text-sm text-muted-foreground">
                {t("apiKeys.keysCreated", { count: keys.length })}
              </p>
            </div>
          )}
          isEmpty={keys.length === 0}
          emptyState={(
            <EmptyState
              icon={<Key className="h-12 w-12" />}
              title={t("apiKeys.yourApiKeys")}
              description={t("apiKeys.noKeys")}
            />
          )}
        >
              <TableVirtuoso
                style={{ height: "calc(100dvh - 280px)", minHeight: 400, overflowX: "auto" }}
                data={keys}
                components={{
                  Table: (props) => (
                    <table
                      {...props}
                      className="w-full caption-bottom text-sm"
                      style={{ minWidth: "72rem" }}
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
                    <VirtualTableHeaderCell className="w-[50px] whitespace-nowrap">
                      <Checkbox
                        checked={selectedKeys.length === keys.length && keys.length > 0}
                        onCheckedChange={toggleSelectAll}
                      />
                    </VirtualTableHeaderCell>
                    <VirtualTableHeaderCell className="whitespace-nowrap">
                      {t("common.name")}
                    </VirtualTableHeaderCell>
                    <VirtualTableHeaderCell className="whitespace-nowrap">
                      {t("apiKeys.keyPrefix")}
                    </VirtualTableHeaderCell>
                    <VirtualTableHeaderCell className="whitespace-nowrap">
                      {t("apiKeys.spendLimits")}
                    </VirtualTableHeaderCell>
                    <VirtualTableHeaderCell className="whitespace-nowrap">
                      {t("apiKeys.restrictions")}
                    </VirtualTableHeaderCell>
                    <VirtualTableHeaderCell className="whitespace-nowrap">
                      {t("apiKeys.expires")}
                    </VirtualTableHeaderCell>
                    <VirtualTableHeaderCell className="whitespace-nowrap">
                      {t("common.status")}
                    </VirtualTableHeaderCell>
                    <VirtualTableHeaderCell className="w-[100px] whitespace-nowrap">
                      {t("common.actions")}
                    </VirtualTableHeaderCell>
                  </tr>
                )}
                itemContent={(_index, key) => (
                  <>
                    <VirtualTableCell>
                      <Checkbox
                        checked={selectedKeys.includes(key.id)}
                        onCheckedChange={() => toggleSelectKey(key.id)}
                      />
                    </VirtualTableCell>
                    <VirtualTableCell className="font-medium">
                      <div className="flex min-w-max items-center gap-2 whitespace-nowrap">
                        <button
                          type="button"
                          className="rounded-md text-left font-medium hover:text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                          onClick={() => setAnalyticsKey(key)}
                        >
                          {key.name}
                        </button>
                        {key.group_ids.length > 0 && (
                          <GroupsBadge groupIds={key.group_ids} variant="secondary" />
                        )}
                        {key.group_ids.length === 0 && (
                          <Badge variant="secondary">{t("apiKeys.allGroups")}</Badge>
                        )}
                      </div>
                    </VirtualTableCell>
                    <VirtualTableCell className="whitespace-nowrap">
                      <div className="flex min-w-max items-center gap-1">
                        <code className="rounded bg-muted px-2 py-0.5 text-sm whitespace-nowrap">
                          {key.key ? `${key.key.slice(0, 12)}...` : `${key.key_prefix}...`}
                        </code>
                        {key.key && (
                          <Button
                            variant="ghost"
                            size="icon"
                            className="size-11 touch-manipulation sm:size-6"
                            aria-label={t("common.copy")}
                            onClick={() => handleCopy(key.key)}
                          >
                            {copiedKey === key.key ? (
                              <Check className="h-3 w-3" />
                            ) : (
                              <Copy className="h-3 w-3" />
                            )}
                          </Button>
                        )}
                      </div>
                    </VirtualTableCell>
                    <VirtualTableCell className="whitespace-nowrap">
                      {key.spend_limit_total_nano_usd || key.spend_limit_hourly_nano_usd || key.spend_limit_daily_nano_usd ? (
                        <div className="flex flex-wrap items-center gap-1">
                          {([
                            ["ALL", key.spend_limit_total_nano_usd, t("orgLimits.windowTotal")],
                            ["1H", key.spend_limit_hourly_nano_usd, t("orgLimits.windowHourly")],
                            ["1D", key.spend_limit_daily_nano_usd, t("orgLimits.windowDaily")],
                          ] as const).map(([tag, nano, label]) =>
                            nano ? (
                              <span
                                key={tag}
                                className="rounded bg-primary/10 px-1.5 py-0.5 font-mono text-xs text-primary"
                                title={label}
                              >
                                {tag} ${tag === "1D" && key.daily_spent_nano_usd
                                  ? `${nanoToUsdInput(key.daily_spent_nano_usd)}/${nanoToUsdInput(nano)}`
                                  : nanoToUsdInput(nano)}
                              </span>
                            ) : null,
                          )}
                        </div>
                      ) : (
                        <span className="text-xs text-muted-foreground">∞</span>
                      )}
                    </VirtualTableCell>
                    <VirtualTableCell className="whitespace-nowrap">
                      <ApiKeyRestrictionBadges apiKey={key} t={t} />
                    </VirtualTableCell>
                    <VirtualTableCell className="whitespace-nowrap">
                      {key.expires_at ? formatDate(key.expires_at) : t("common.never")}
                    </VirtualTableCell>
                    <VirtualTableCell className="whitespace-nowrap">
                      <Switch
                        checked={key.enabled}
                        onCheckedChange={() => handleToggleEnabled(key)}
                      />
                    </VirtualTableCell>
                    <VirtualTableCell>
                      <div className="flex gap-1">
                        <Button
                          variant="ghost"
                          size="icon"
                          className="size-11 touch-manipulation sm:size-9"
                          aria-label={t("apiKeys.analyticsTitle")}
                          onClick={() => setAnalyticsKey(key)}
                        >
                          <BarChart3 className="h-4 w-4" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon"
                          className="size-11 touch-manipulation sm:size-9"
                          aria-label={t("common.edit")}
                          onClick={() => openEditDialog(key)}
                        >
                          <Edit className="h-4 w-4" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon"
                          aria-label={t("common.delete")}
                          onClick={() => handleDelete(key.id)}
                          className="size-11 touch-manipulation sm:size-9 text-destructive hover:text-destructive"
                        >
                          <Trash2 className="h-4 w-4" />
                        </Button>
                      </div>
                    </VirtualTableCell>
                  </>
                )}
              />
        </DataTableShell>
      </motion.div>

      <ApiKeyAnalyticsDialog
        apiKey={analyticsKey}
        onOpenChange={(open) => {
          if (!open) setAnalyticsKey(null);
        }}
      />

      {/* Edit Dialog */}
      <Dialog open={!!editKey} onOpenChange={(open) => { if (!open) { setEditKey(null); resetCreateForm(); } }}>
        <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-hidden p-0 sm:max-h-[calc(100dvh-3rem)] sm:max-w-4xl">
          <div className="flex min-h-0 flex-col p-6">
          <DialogHeader className="shrink-0">
            <DialogTitle>{t("apiKeys.editApiKey")}</DialogTitle>
            <DialogDescription>
              {t("apiKeys.editDescription")}
            </DialogDescription>
          </DialogHeader>
          <div className="min-h-0 flex-1 space-y-4 overflow-y-auto py-4 pr-1">
            <div className="space-y-2">
              <Label htmlFor="editName">{t("common.name")}</Label>
              <Input
                id="editName"
                value={newKeyName}
                onChange={(e) => setNewKeyName(e.target.value)}
              />
            </div>
            <KeyGroupsSection
              idPrefix="edit"
              groupIds={newKeyGroupIds}
              groups={groups}
              groupsLoading={groupsLoading}
              ownerGroupId={currentUser?.group_id ?? null}
              isAdmin={canManageSystem}
              onGroupIdsChange={setNewKeyGroupIds}
            />
            <ModelBindingsSection
              conflicts={modelConflicts}
              loading={modelConflictsLoading}
              groupIds={newKeyGroupIds}
              modelLimitsEnabled={newKeyModelLimitsEnabled}
              modelLimits={newKeyModelLimits.split(",").map((model) => model.trim()).filter(Boolean)}
              value={newKeyModelBindings}
              onChange={setNewKeyModelBindings}
              failed={Boolean(modelConflictsError)}
            />
            <ChannelBindingsSection
              conflicts={channelConflicts}
              loading={channelConflictsLoading}
              groupIds={newKeyGroupIds}
              modelLimitsEnabled={newKeyModelLimitsEnabled}
              modelLimits={newKeyModelLimits.split(",").map((model) => model.trim()).filter(Boolean)}
              value={newKeyChannelBindings}
              onChange={setNewKeyChannelBindings}
              failed={Boolean(channelConflictsError)}
            />
            <SpendLimitsEditor
              idPrefix="edit"
              draft={newKeySpendLimits}
              currency={newKeyLimitCurrency}
              onCurrencyChange={setNewKeyLimitCurrency}
              onChange={setNewKeySpendLimits}
              cnyPerUsd={exchangeRate?.cny_per_usd}
              storedNano={{
                total: editKey?.spend_limit_total_nano_usd,
                hourly: editKey?.spend_limit_hourly_nano_usd,
                daily: editKey?.spend_limit_daily_nano_usd,
              }}
            />
            <div className="space-y-1">
              <div className="flex items-center space-x-2">
                <Switch
                  id="editReasoningEnvelopeEnabled"
                  checked={newKeyReasoningEnvelopeEnabled}
                  onCheckedChange={setNewKeyReasoningEnvelopeEnabled}
                />
                <Label htmlFor="editReasoningEnvelopeEnabled">{t("apiKeys.reasoningEnvelopeEnabled")}</Label>
              </div>
              <p className="text-sm text-muted-foreground">{t("apiKeys.reasoningEnvelopeHelp")}</p>
            </div>
                <div className="space-y-1">
                  <div className="flex items-center space-x-2">
                <Label htmlFor="editRequestCaptureMode">{t("apiKeys.requestCaptureMode")}</Label>
              </div>
              <Select value={newKeyRequestCaptureMode} onValueChange={(value) => setNewKeyRequestCaptureMode(value as RequestCaptureMode)}>
                <SelectTrigger id="editRequestCaptureMode">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="off">{t("apiKeys.requestCaptureModeOff")}</SelectItem>
                  <SelectItem value="capture-all">{t("apiKeys.requestCaptureModeAll")}</SelectItem>
                  <SelectItem value="capture-only-abnormal">{t("apiKeys.requestCaptureModeAbnormal")}</SelectItem>
                </SelectContent>
              </Select>
              <p className="text-sm text-muted-foreground">{t("apiKeys.requestCaptureHelp")}</p>
            </div>
            <div className="flex items-center space-x-2">
              <Switch
                id="editModelLimitsEnabled"
                checked={newKeyModelLimitsEnabled}
                onCheckedChange={setNewKeyModelLimitsEnabled}
              />
              <Label htmlFor="editModelLimitsEnabled">{t("apiKeys.enableModelLimits")}</Label>
            </div>
            {newKeyModelLimitsEnabled && (
              <div className="space-y-2">
                <Label htmlFor="editModelLimits">{t("apiKeys.allowedModels")}</Label>
                <Input
                  id="editModelLimits"
                  value={newKeyModelLimits}
                  onChange={(e) => setNewKeyModelLimits(e.target.value)}
                />
              </div>
            )}
            <div className="space-y-2">
              <Label htmlFor="editIpWhitelist">{t("apiKeys.ipWhitelist")}</Label>
              <Input
                id="editIpWhitelist"
                value={newKeyIpWhitelist}
                onChange={(e) => setNewKeyIpWhitelist(e.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="editMaxMultiplier">{t("apiKeys.maxMultiplier")}</Label>
              <Input
                id="editMaxMultiplier"
                type="text"
                inputMode="decimal"
                value={newKeyMaxMultiplier}
                onChange={(e) => setNewKeyMaxMultiplier(e.target.value)}
                placeholder="e.g. 1.5"
              />
              <p className="text-sm text-muted-foreground">{t("apiKeys.maxMultiplierHelp")}</p>
            </div>
            <div className="space-y-3">
              <div className="flex items-center gap-2">
                <Settings2 className="h-4 w-4 text-muted-foreground" />
                <h3 className="text-sm font-medium">{t("transforms.titleApiKey")}</h3>
              </div>
              <TransformChainEditor
                value={newKeyTransforms}
                registry={apiKeyTransformRegistry}
                loading={transformRegistryLoading}
                onChange={setNewKeyTransforms}
              />
            </div>
            <ModelRedirectsEditor
              value={newKeyModelRedirects}
              onChange={setNewKeyModelRedirects}
            />
          </div>
          <DialogFooter className="shrink-0 pt-4">
            <Button variant="outline" onClick={() => { setEditKey(null); resetCreateForm(); }}>
              {t("common.cancel")}
            </Button>
            <Button onClick={handleUpdate} disabled={updating || channelSelectionBlocked}>
              {updating ? t("common.saving") : t("common.save")}
            </Button>
          </DialogFooter>
          </div>
        </DialogContent>
      </Dialog>

      <AlertDialog open={!!deleteTargetId} onOpenChange={(open) => { if (!open) setDeleteTargetId(null); }}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("apiKeys.confirmDeleteTitle")}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("apiKeys.confirmDelete")}
            </AlertDialogDescription>
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

      <AlertDialog open={batchDeleteOpen} onOpenChange={(open) => { if (!open) setBatchDeleteOpen(false); }}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("apiKeys.confirmBatchDelete", { count: selectedKeys.length })}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("apiKeys.confirmBatchDeleteDesc", { count: selectedKeys.length })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              onClick={confirmBatchDelete}
            >
              {t("common.delete")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

    </PageWrapper>
  );
}
