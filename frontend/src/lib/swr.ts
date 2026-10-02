import useSWR, { mutate } from "swr";
import type { Key, SWRConfiguration } from "swr";
import { api } from "./api";
import type {
  User,
  ApiKey,
  ApiKeyAnalytics,
  ApiKeyAnalyticsRange,
  DashboardStats,
  DashboardAnalytics,
  AdminOverview,
  AdminUsageRanking,
  AdminAnnouncementsResponse,
  AdminRevenueDaily,
  AdminRevenueExclusions,
  AnnouncementsResponse,
  PublicUsageRanking,
  UsageRankingRange,
  ConfigOverview,
  SystemSettings,
  PublicSystemSettings,
  PublicSiteSettings,
  Provider,
  CreateProviderInput,
  CreateWholesaleProviderInput,
  UpdateProviderInput,
  CreateApiKeyInput,
  UpdateApiKeyInput,
  TransformRegistryItem,
  RequestLogsResponse,
  RequestLogsFilter,
  RequestCaptureDetail,
  ModelMetadataRecord,
  UpsertModelMetadataInput,
  BillingRateProfileSummary,
  DeletePricingProfileResult,
  BillingRateRecord,
  UpsertBillingRateInput,
  PricingProfilePattern,
  BillingPlan,
  BillingPlanInput,
  Group,
  CreateGroupInput,
  UpdateGroupInput,
  UserLiveUsage,
  ApiKeyChannelConflict,
  ApiKeyModelConflict,
  AccountClass,
  FirewallStats,
  FirewallEventsResponse,
  FirewallEventsFilter,
} from "./api";

// SWR fetcher functions
const fetchers = {
  me: () => api.me(),
  users: () => api.listUsers(),
  apiKeys: () => api.listApiKeys(),
  apiKeyChannelConflicts: () => api.listApiKeyChannelConflicts(),
  apiKeyModelConflicts: () => api.listApiKeyModelConflicts(),
  stats: () => api.getStats(),
  config: () => api.getConfigOverview(),
  settings: () => api.getSettings(),
  publicSettings: () => api.getPublicSettings(),
  publicSiteSettings: () => api.getPublicSiteSettings(),
  providers: () => api.listProviders(),
  dashboardGroups: async () => (await api.listDashboardGroups()).groups,
  transformRegistry: () => api.getTransformRegistry(),
  modelMetadata: () => api.listModelMetadata(),
  modelMetadataDetail: (modelId: string) => api.getModelMetadata(modelId),
  billingRates: () => api.listBillingRates(),
  billingRatesForProfile: (profile: string) => api.listBillingRatesForProfile(profile),
  billingRateProfiles: async () => (await api.listBillingRateProfiles()).profiles,
  billingPlans: () => api.listBillingPlans(),
  pricingProfilePatterns: async () => (await api.getPricingProfilePatterns()).patterns,
  marketplaceModels: (groupId?: string) => api.listMarketplaceModels(groupId),
};

// SWR cache keys
export const SWR_KEYS = {
  ME: "/dashboard/me",
  USERS: "/dashboard/users",
  API_KEYS: "/dashboard/tokens",
  API_KEY_CHANNEL_CONFLICTS: "/dashboard/tokens/channel-conflicts",
  API_KEY_MODEL_CONFLICTS: "/dashboard/tokens/model-conflicts",
  STATS: "/dashboard/stats",
  CONFIG: "/dashboard/config",
  SETTINGS: "/dashboard/settings",
  PUBLIC_SETTINGS: "/dashboard/public-settings",
  PUBLIC_SITE_SETTINGS: "/api/public/site",
  PUBLIC_USAGE_RANKING: "/api/public/usage-ranking",
  PROVIDERS: "/dashboard/providers",
  DASHBOARD_GROUPS: "/dashboard/groups",
  TRANSFORM_REGISTRY: "/dashboard/transforms/registry",
  MODEL_METADATA: "/dashboard/model-metadata",
  BILLING_RATES: "/dashboard/billing-rates",
  BILLING_RATE_PROFILES: "/dashboard/billing-rates/profiles",
  PRICING_PROFILE_PATTERNS: "/dashboard/pricing-profile-patterns",
  MARKETPLACE_MODELS: "/dashboard/marketplace/models",
  BILLING_PLANS: "/dashboard/billing-plans",
  REQUEST_LOGS: "/dashboard/request-logs",
  ANALYTICS: "/dashboard/analytics",
  ADMIN_OVERVIEW: "/dashboard/admin/overview",
  ADMIN_USAGE: "/dashboard/admin/usage-ranking",
  ADMIN_REVENUE_DAILY: "/dashboard/admin/revenue/daily",
  ADMIN_REVENUE_EXCLUSIONS: "/dashboard/admin/revenue/exclusions",
  ANNOUNCEMENTS: "/dashboard/announcements",
  ADMIN_ANNOUNCEMENTS: "/dashboard/announcements/admin",
  LIVE_USAGE: "/dashboard/me/live-usage",
  FIREWALL_STATS: "/dashboard/firewall/stats",
  FIREWALL_EVENTS: "/dashboard/firewall/events",
} as const;

export function providerDetailSWRKey(providerId: string) {
  return `provider-detail:${providerId}`;
}

export function modelMetadataDetailSWRKey(modelId: string) {
  return `model-metadata-detail:${modelId}`;
}

export function billingRatesForProfileSWRKey(profile: string) {
  return `billing-rates-profile:${profile}`;
}

function revalidateBillingRateKeys() {
  // DC9: rate writes can change every per-profile view plus the summary counts.
  return mutate((key) => typeof key === "string" && key.startsWith("billing-rates-profile:"));
}

function isBillingRateCacheKey(key: Key) {
  return (
    key === SWR_KEYS.BILLING_RATES ||
    (typeof key === "string" && key.startsWith("billing-rates-profile:"))
  );
}

/**
 * Optimistically rewrites every cached billing-rate view (flat catalog and profile-scoped)
 * in place. Matching by cache key keeps a profile-scoped writer from planting partial data
 * under the unmounted flat key.
 */
function mutateCachedBillingRates(
  apply: (records: BillingRateRecord[]) => BillingRateRecord[]
) {
  return mutate(
    isBillingRateCacheKey,
    (records?: BillingRateRecord[]) => (records ? apply(records) : records),
    false
  );
}

async function upsertCachedBillingRate(record: BillingRateRecord) {
  const profileKey = billingRatesForProfileSWRKey(record.pricing_profile);
  const snapshots = new Map<BillingRateRecord[], BillingRateRecord[]>();
  const remember = (before: BillingRateRecord[], after: BillingRateRecord[]) => {
    snapshots.set(after, before);
    return after;
  };
  await Promise.all([
    mutate(
      (key) => key === SWR_KEYS.BILLING_RATES || key === profileKey,
      (records?: BillingRateRecord[]) => {
        if (!records) return records;
        const updated = records.some((row) => row.id === record.id)
          ? records.map((row) => (row.id === record.id ? record : row))
          : [...records, record];
        return remember(records, updated);
      },
      false
    ),
    mutate(
      (key) => isBillingRateCacheKey(key) && key !== SWR_KEYS.BILLING_RATES && key !== profileKey,
      (records?: BillingRateRecord[]) => {
        if (!records?.some((row) => row.id === record.id)) return records;
        return remember(records, records.filter((row) => row.id !== record.id));
      },
      false
    ),
  ]);
  // Restore only our exact arrays; a later writer owns any replacement array.
  return () => mutateCachedBillingRates((records) => snapshots.get(records) ?? records);
}

// Default SWR config
const defaultConfig: SWRConfiguration = {
  revalidateOnFocus: true,
  revalidateOnReconnect: true,
  dedupingInterval: 2000,
};

// Current user hook
export function useCurrentUser(config?: SWRConfiguration) {
  return useSWR<User>(
    SWR_KEYS.ME,
    fetchers.me,
    { ...defaultConfig, ...config }
  );
}

// Users list hook (admin only)
export function useUsers(config?: SWRConfiguration) {
  return useSWR<User[]>(SWR_KEYS.USERS, fetchers.users, {
    ...defaultConfig,
    ...config,
  });
}

// API keys hook
export function useApiKeys(config?: SWRConfiguration) {
  return useSWR<ApiKey[]>(SWR_KEYS.API_KEYS, fetchers.apiKeys, {
    ...defaultConfig,
    ...config,
  });
}

export function useApiKeyAnalytics(
  keyId: string | null,
  range: ApiKeyAnalyticsRange,
  config?: SWRConfiguration<ApiKeyAnalytics>,
) {
  return useSWR<ApiKeyAnalytics>(
    keyId ? ["api-key-analytics", keyId, range] : null,
    () => api.getApiKeyAnalytics(keyId!, range),
    {
      ...defaultConfig,
      keepPreviousData: true,
      refreshInterval: 10_000,
      ...config,
    },
  );
}

export function useApiKeyChannelConflicts(config?: SWRConfiguration) {
  return useSWR<ApiKeyChannelConflict[]>(
    SWR_KEYS.API_KEY_CHANNEL_CONFLICTS,
    fetchers.apiKeyChannelConflicts,
    { ...defaultConfig, ...config },
  );
}

export function useApiKeyModelConflicts(config?: SWRConfiguration) {
  return useSWR<ApiKeyModelConflict[]>(
    SWR_KEYS.API_KEY_MODEL_CONFLICTS,
    fetchers.apiKeyModelConflicts,
    { ...defaultConfig, ...config },
  );
}

// Billing plans hook (admin only)
export function useBillingPlans(config?: SWRConfiguration) {
  return useSWR<BillingPlan[]>(SWR_KEYS.BILLING_PLANS, fetchers.billingPlans, {
    ...defaultConfig,
    ...config,
  });
}

// Dashboard stats hook
export function useStats(config?: SWRConfiguration) {
  return useSWR<DashboardStats>(SWR_KEYS.STATS, fetchers.stats, {
    ...defaultConfig,
    ...config,
  });
}

// Config overview hook (admin only)
export function useConfigOverview(config?: SWRConfiguration) {
  return useSWR<ConfigOverview>(SWR_KEYS.CONFIG, fetchers.config, {
    ...defaultConfig,
    ...config,
  });
}

// System settings hook (admin only)
export function useSettings(config?: SWRConfiguration) {
  return useSWR<SystemSettings>(SWR_KEYS.SETTINGS, fetchers.settings, {
    ...defaultConfig,
    ...config,
  });
}

// Public settings hook (no auth required)
export function usePublicSettings(config?: SWRConfiguration) {
  return useSWR<PublicSystemSettings>(
    SWR_KEYS.PUBLIC_SETTINGS,
    fetchers.publicSettings,
    { ...defaultConfig, ...config }
  );
}

export function usePublicSiteSettings(config?: SWRConfiguration) {
  return useSWR<PublicSiteSettings>(
    SWR_KEYS.PUBLIC_SITE_SETTINGS,
    fetchers.publicSiteSettings,
    { ...defaultConfig, ...config }
  );
}

// Providers hook (admin only)
export function useProviders(config?: SWRConfiguration, enabled = true) {
  return useSWR<Provider[]>(enabled ? SWR_KEYS.PROVIDERS : null, fetchers.providers, {
    ...defaultConfig,
    ...config,
  });
}

export function useProviderDetail(
  providerId: string | null | undefined,
  config?: SWRConfiguration
) {
  return useSWR<Provider>(
    providerId ? providerDetailSWRKey(providerId) : null,
    () => api.getProvider(providerId!),
    { ...defaultConfig, ...config }
  );
}

export function useDashboardGroups(enabled = true, config?: SWRConfiguration) {
  return useSWR<Group[]>(
    enabled ? SWR_KEYS.DASHBOARD_GROUPS : null,
    fetchers.dashboardGroups,
    {
      ...defaultConfig,
      ...config,
    }
  );
}

export function useTransformRegistry(config?: SWRConfiguration) {
  return useSWR<TransformRegistryItem[]>(
    SWR_KEYS.TRANSFORM_REGISTRY,
    fetchers.transformRegistry,
    {
      ...defaultConfig,
      ...config,
    }
  );
}

export function useModelMetadata(config?: SWRConfiguration) {
  return useSWR<ModelMetadataRecord[]>(
    SWR_KEYS.MODEL_METADATA,
    fetchers.modelMetadata,
    { ...defaultConfig, ...config }
  );
}

export function useBillingRates(config?: SWRConfiguration) {
  return useSWR<BillingRateRecord[]>(
    SWR_KEYS.BILLING_RATES,
    fetchers.billingRates,
    { ...defaultConfig, ...config }
  );
}

export function useBillingRatesForProfile(
  profile: string | null | undefined,
  config?: SWRConfiguration
) {
  return useSWR<BillingRateRecord[]>(
    profile ? billingRatesForProfileSWRKey(profile) : null,
    () => fetchers.billingRatesForProfile(profile!),
    { ...defaultConfig, ...config }
  );
}

export function useBillingRateProfiles(config?: SWRConfiguration) {
  return useSWR<BillingRateProfileSummary[]>(
    SWR_KEYS.BILLING_RATE_PROFILES,
    fetchers.billingRateProfiles,
    { ...defaultConfig, ...config }
  );
}

export function useModelMetadataDetail(
  modelId: string | null | undefined,
  config?: SWRConfiguration
) {
  return useSWR<ModelMetadataRecord>(
    modelId ? modelMetadataDetailSWRKey(modelId) : null,
    () => fetchers.modelMetadataDetail(modelId!),
    { ...defaultConfig, ...config }
  );
}

export function usePricingProfilePatterns(config?: SWRConfiguration) {
  return useSWR<PricingProfilePattern[]>(
    SWR_KEYS.PRICING_PROFILE_PATTERNS,
    fetchers.pricingProfilePatterns,
    { ...defaultConfig, ...config }
  );
}

export function useMarketplaceModels(groupId?: string, config?: SWRConfiguration) {
  return useSWR<ModelMetadataRecord[]>(
    groupId ? `${SWR_KEYS.MARKETPLACE_MODELS}?group_id=${groupId}` : SWR_KEYS.MARKETPLACE_MODELS,
    () => fetchers.marketplaceModels(groupId),
    { ...defaultConfig, ...config }
  );
}

export function useFirewallStats(config?: SWRConfiguration) {
  return useSWR<FirewallStats>(
    SWR_KEYS.FIREWALL_STATS,
    () => api.getFirewallStats(),
    { ...defaultConfig, ...config }
  );
}

export function useFirewallEvents(
  limit = 50,
  offset = 0,
  filters?: FirewallEventsFilter,
  config?: SWRConfiguration
) {
  const filterKey = filters ? JSON.stringify(filters) : "";
  return useSWR<FirewallEventsResponse>(
    `${SWR_KEYS.FIREWALL_EVENTS}?limit=${limit}&offset=${offset}&f=${filterKey}`,
    () => api.listFirewallEvents(limit, offset, filters),
    { ...defaultConfig, ...config }
  );
}

export function useRequestLogs(limit = 50, offset = 0, filters?: RequestLogsFilter, config?: SWRConfiguration) {
  const filterKey = filters ? JSON.stringify(filters) : "";
  return useSWR<RequestLogsResponse>(
    `${SWR_KEYS.REQUEST_LOGS}?limit=${limit}&offset=${offset}&f=${filterKey}`,
    () => api.listRequestLogs(limit, offset, filters),
    { ...defaultConfig, ...config }
  );
}

// Capture detail hook (request-capture-viewer.spec.md RCV-F5): the key is
// null until `enabled` is true, so no fetch happens before the dialog opens;
// the cache entry survives close/reopen per normal SWR semantics.
export function useRequestCapture(
  requestId: string | null | undefined,
  userId: string | null | undefined,
  enabled: boolean,
  config?: SWRConfiguration
) {
  return useSWR<RequestCaptureDetail>(
    enabled && requestId
      ? `${SWR_KEYS.REQUEST_LOGS}/capture?rid=${encodeURIComponent(requestId)}&uid=${encodeURIComponent(userId ?? "")}`
      : null,
    () => api.getRequestCapture(requestId as string, userId ?? undefined),
    { ...defaultConfig, revalidateOnFocus: false, ...config }
  );
}

export function useDashboardAnalytics(
  buckets = 8,
  rangeHours = 24,
  scope?: "self",
  config?: SWRConfiguration,
) {
  const scopeQuery = scope ? `&scope=${scope}` : "";
  return useSWR<DashboardAnalytics>(
    `${SWR_KEYS.ANALYTICS}?buckets=${buckets}&range_hours=${rangeHours}${scopeQuery}`,
    () => api.getDashboardAnalytics(buckets, rangeHours, scope),
    { ...defaultConfig, ...config }
  );
}

// Own rolling 60-second usage hook (user-live-usage.spec.md LU-9/LU-10).
// Mounted only while the user-center dropdown is open, so the 10s poll
// runs only while the menu is visible.
export function useLiveUsage(config?: SWRConfiguration) {
  return useSWR<UserLiveUsage>(SWR_KEYS.LIVE_USAGE, () => api.getMyLiveUsage(), {
    ...defaultConfig,
    refreshInterval: 10000,
    ...config,
  });
}

// Admin overview hook (admin dashboard; AD-ADF-7: 10s refresh, skeleton, retry)
export function useAdminOverview(config?: SWRConfiguration) {
  return useSWR<AdminOverview>(SWR_KEYS.ADMIN_OVERVIEW, () => api.getAdminOverview(), {
    ...defaultConfig,
    refreshInterval: 10000,
    ...config
  });
}

export function useAdminUsageRanking(range: UsageRankingRange = "today", config?: SWRConfiguration) {
  return useSWR<AdminUsageRanking>(`${SWR_KEYS.ADMIN_USAGE}?range=${range}`, () => api.getAdminUsageRanking(range), {
    ...defaultConfig,
    refreshInterval: 10000,
    ...config,
  });
}

export function adminRevenueDailySWRKey(from: string, to: string) {
  return `${SWR_KEYS.ADMIN_REVENUE_DAILY}?from=${from}&to=${to}`;
}

export function useAdminRevenueDaily(from: string, to: string, config?: SWRConfiguration) {
  return useSWR<AdminRevenueDaily>(adminRevenueDailySWRKey(from, to), () => api.getAdminRevenueDaily(from, to), {
    ...defaultConfig,
    refreshInterval: 10_000,
    ...config,
  });
}

export function useAdminRevenueExclusions(config?: SWRConfiguration) {
  return useSWR<AdminRevenueExclusions>(SWR_KEYS.ADMIN_REVENUE_EXCLUSIONS, () => api.listAdminRevenueExclusions(), {
    ...defaultConfig,
    ...config,
  });
}

export function useAnnouncements(config?: SWRConfiguration) {
  return useSWR<AnnouncementsResponse>(SWR_KEYS.ANNOUNCEMENTS, () => api.listAnnouncements(), {
    ...defaultConfig,
    // AN-14: poll the bell data while the dashboard layout is mounted.
    refreshInterval: 60_000,
    ...config,
  });
}

export function useAdminAnnouncements(config?: SWRConfiguration) {
  return useSWR<AdminAnnouncementsResponse>(SWR_KEYS.ADMIN_ANNOUNCEMENTS, () => api.listAdminAnnouncements(), {
    ...defaultConfig,
    ...config,
  });
}

export function usePublicUsageRanking(range: UsageRankingRange, config?: SWRConfiguration) {
  const key = `${SWR_KEYS.PUBLIC_USAGE_RANKING}?range=${range}`;
  return useSWR<PublicUsageRanking>(key, () => api.getPublicUsageRanking(range), {
    ...defaultConfig,
    refreshInterval: 10000,
    keepPreviousData: true,
    ...config,
  });
}

// Mutation helpers with optimistic updates

export async function updateMeOptimistic(
  updates: { email?: string | null; usage_ranking_anonymous?: boolean },
  currentUser: User | undefined,
  onError?: (error: Error) => void
) {
  if (currentUser) {
    mutate(SWR_KEYS.ME, { ...currentUser, ...updates }, false);
  }

  try {
    const updated = await api.updateMe(updates);
    mutate(SWR_KEYS.ME, updated, false);
    return updated;
  } catch (error) {
    mutate(SWR_KEYS.ME);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function updateSettingsOptimistic(
  newSettings: SystemSettings,
  onError?: (error: Error) => void
) {
  // Optimistic update
  mutate(SWR_KEYS.SETTINGS, newSettings, false);

  try {
    const updated = await api.updateSettings(newSettings);
    // Revalidate with server data
    mutate(SWR_KEYS.SETTINGS, updated, false);
    mutate(SWR_KEYS.PUBLIC_SETTINGS);
    mutate(SWR_KEYS.PRICING_PROFILE_PATTERNS);
    mutate(SWR_KEYS.PROVIDERS);
    return updated;
  } catch (error) {
    // Rollback on error
    mutate(SWR_KEYS.SETTINGS);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function createUserOptimistic(
  username: string,
  password: string,
  role: string,
  groupId: string | undefined,
  currentUsers: User[],
  onError?: (error: Error) => void
) {
  // Optimistic update with placeholder
  const tempUser: User = {
    id: `temp-${Date.now()}`,
    username,
    role: role as User["role"],
    // The server creates every user as standard, so the placeholder must match or the edit
    // dialog renders an account class the row does not have.
    account_class: "standard",
    // A sales agent is only ever created through the sales endpoint, never here.
    is_sales_agent: false,
    enabled: true,
    created_at: new Date().toISOString(),
    last_login_at: undefined,
    balance_nano_usd: "0",
    balance_usd: "0",
    balance_unlimited: false,
    usage_ranking_anonymous: true,
    group_id: groupId ?? "",
    billing_plan_id: null,
    next_grant_at: null,
    billing_plan: null,
    today_calls: 0,
    today_cost_nano_usd: "0",
    today_cost_usd: "0",
  };
  mutate(SWR_KEYS.USERS, [...currentUsers, tempUser], false);

  try {
    await api.createUser(username, password, role, groupId);
    // Revalidate to get the real user data
    mutate(SWR_KEYS.USERS);
    mutate(SWR_KEYS.STATS);
  } catch (error) {
    // Rollback on error
    mutate(SWR_KEYS.USERS, currentUsers, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function updateUserOptimistic(
  userId: string,
  updates: Partial<User> & { password?: string },
  currentUsers: User[],
  onError?: (error: Error) => void
) {
  // Optimistic update
  const updatedUsers = currentUsers.map((u) =>
    u.id === userId ? { ...u, ...updates } : u
  );
  mutate(SWR_KEYS.USERS, updatedUsers, false);

  try {
    await api.updateUser(userId, updates);
    // Revalidate to get the real data
    mutate(SWR_KEYS.USERS);
    mutate(SWR_KEYS.ME);
    mutate(SWR_KEYS.STATS);
  } catch (error) {
    // Rollback on error
    mutate(SWR_KEYS.USERS, currentUsers, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function updateUserAccountClassOptimistic(
  userId: string,
  accountClass: AccountClass,
  currentUsers: User[],
  onError?: (error: Error) => void,
) {
  const optimistic = currentUsers.map((user) =>
    user.id === userId ? { ...user, account_class: accountClass } : user,
  );
  mutate(SWR_KEYS.USERS, optimistic, false);
  try {
    const updated = await api.updateUserAccountClass(userId, accountClass);
    await Promise.all([
      mutate(SWR_KEYS.USERS),
      mutate(SWR_KEYS.ME),
      mutate(SWR_KEYS.API_KEYS),
      mutate(SWR_KEYS.DASHBOARD_GROUPS),
      mutate(SWR_KEYS.PROVIDERS),
      mutate(SWR_KEYS.MARKETPLACE_MODELS),
      mutate(SWR_KEYS.STATS),
      mutate(SWR_KEYS.ANALYTICS),
    ]);
    return updated;
  } catch (error) {
    mutate(SWR_KEYS.USERS, currentUsers, false);
    if (onError && error instanceof Error) onError(error);
    throw error;
  }
}

export async function deleteUserOptimistic(
  userId: string,
  currentUsers: User[],
  onError?: (error: Error) => void
) {
  // Optimistic update
  const filteredUsers = currentUsers.filter((u) => u.id !== userId);
  mutate(SWR_KEYS.USERS, filteredUsers, false);

  try {
    await api.deleteUser(userId);
    // Revalidate
    mutate(SWR_KEYS.USERS);
    mutate(SWR_KEYS.STATS);
  } catch (error) {
    // Rollback on error
    mutate(SWR_KEYS.USERS, currentUsers, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

// Group registry mutation helpers

function sortGroups(groups: Group[]): Group[] {
  return [...groups].sort(
    (a, b) =>
      a.sort_order - b.sort_order ||
      a.created_at.localeCompare(b.created_at) ||
      a.id.localeCompare(b.id)
  );
}

export async function createGroupOptimistic(
  input: CreateGroupInput,
  currentGroups: Group[],
  onError?: (error: Error) => void
) {
  const now = new Date().toISOString();
  const tempGroup: Group = {
    id: `temp-${Date.now()}`,
    name: input.name.trim(),
    description: (input.description ?? "").trim(),
    is_default: false,
    user_selectable: input.user_selectable ?? false,
    is_public: input.is_public ?? true,
    sort_order: input.sort_order ?? 0,
    created_at: now,
    updated_at: now,
    account_class: input.account_class ?? "standard",
  };
  mutate(SWR_KEYS.DASHBOARD_GROUPS, sortGroups([...currentGroups, tempGroup]), false);

  try {
    const created = await api.createGroup(input);
    mutate(SWR_KEYS.DASHBOARD_GROUPS);
    return created;
  } catch (error) {
    mutate(SWR_KEYS.DASHBOARD_GROUPS, currentGroups, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function updateGroupOptimistic(
  groupId: string,
  input: UpdateGroupInput,
  currentGroups: Group[],
  onError?: (error: Error) => void
) {
  const optimistic = sortGroups(
    currentGroups.map((g) =>
      g.id === groupId
        ? {
            ...g,
            name: input.name ?? g.name,
            description: input.description ?? g.description,
            user_selectable: input.user_selectable ?? g.user_selectable,
            is_public: input.is_public ?? g.is_public,
            sort_order: input.sort_order ?? g.sort_order,
            updated_at: new Date().toISOString(),
          }
        : g
    )
  );
  mutate(SWR_KEYS.DASHBOARD_GROUPS, optimistic, false);

  try {
    const updated = await api.updateGroup(groupId, input);
    mutate(SWR_KEYS.DASHBOARD_GROUPS);
    return updated;
  } catch (error) {
    mutate(SWR_KEYS.DASHBOARD_GROUPS, currentGroups, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function reorderGroupsOptimistic(
  groupIds: string[],
  currentGroups: Group[],
  onError?: (error: Error) => void
) {
  const groupsById = new Map(currentGroups.map((group) => [group.id, group]));
  const updatedAt = new Date().toISOString();
  const reordered = groupIds.flatMap((groupId, sortOrder) => {
    const group = groupsById.get(groupId);
    return group
      ? [{ ...group, sort_order: sortOrder, updated_at: updatedAt }]
      : [];
  });
  const reorderedIds = new Set(groupIds);
  const optimistic = sortGroups([
    ...currentGroups.filter((group) => !reorderedIds.has(group.id)),
    ...reordered,
  ]);
  mutate(SWR_KEYS.DASHBOARD_GROUPS, optimistic, false);

  try {
    await api.reorderGroups(groupIds);
    mutate(SWR_KEYS.DASHBOARD_GROUPS);
  } catch (error) {
    mutate(SWR_KEYS.DASHBOARD_GROUPS, currentGroups, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function deleteGroupOptimistic(
  groupId: string,
  currentGroups: Group[],
  onError?: (error: Error) => void
) {
  mutate(
    SWR_KEYS.DASHBOARD_GROUPS,
    currentGroups.filter((g) => g.id !== groupId),
    false
  );

  try {
    await api.deleteGroup(groupId);
    // Deletion cascades to users, API keys, providers, and billing plans.
    mutate(SWR_KEYS.DASHBOARD_GROUPS);
    mutate(SWR_KEYS.USERS);
    mutate(SWR_KEYS.API_KEYS);
    mutate(SWR_KEYS.PROVIDERS);
    mutate(SWR_KEYS.BILLING_PLANS);
    mutate(SWR_KEYS.ME);
  } catch (error) {
    mutate(SWR_KEYS.DASHBOARD_GROUPS, currentGroups, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

const NANO_PER_USD = 1_000_000_000n;

function formatNanoToUsd(nano: bigint): string {
  const negative = nano < 0n;
  const abs = negative ? -nano : nano;
  const whole = abs / NANO_PER_USD;
  const frac = abs % NANO_PER_USD;
  if (frac === 0n) {
    return `${negative ? "-" : ""}${whole.toString()}`;
  }
  const fracStr = frac.toString().padStart(9, "0").replace(/0+$/, "");
  return `${negative ? "-" : ""}${whole.toString()}.${fracStr}`;
}

function optimisticGrantFields(input: BillingPlanInput): {
  grant_amount_nano_usd: string;
  grant_amount_usd: string;
} {
  if (input.grant_amount_nano_usd !== undefined) {
    let usd = input.grant_amount_usd;
    if (usd === undefined) {
      try {
        usd = formatNanoToUsd(BigInt(input.grant_amount_nano_usd));
      } catch {
        usd = "0";
      }
    }
    return {
      grant_amount_nano_usd: input.grant_amount_nano_usd,
      grant_amount_usd: usd,
    };
  }
  if (input.grant_amount_usd !== undefined) {
    return {
      grant_amount_nano_usd: "0",
      grant_amount_usd: input.grant_amount_usd,
    };
  }
  return { grant_amount_nano_usd: "0", grant_amount_usd: "0" };
}

export async function createBillingPlanOptimistic(
  input: BillingPlanInput,
  currentPlans: BillingPlan[],
  onError?: (error: Error) => void
) {
  const amounts = optimisticGrantFields(input);
  const tempPlan: BillingPlan = {
    id: `temp-${Date.now()}`,
    name: input.name,
    grant_amount_nano_usd: amounts.grant_amount_nano_usd,
    grant_amount_usd: amounts.grant_amount_usd,
    schedule: input.schedule,
    group_ids: input.group_ids ?? [],
    enabled: input.enabled ?? true,
    created_at: new Date().toISOString(),
    updated_at: new Date().toISOString(),
  };
  mutate(SWR_KEYS.BILLING_PLANS, [...currentPlans, tempPlan], false);

  try {
    await api.createBillingPlan(input);
    mutate(SWR_KEYS.BILLING_PLANS);
    mutate(SWR_KEYS.USERS);
  } catch (error) {
    mutate(SWR_KEYS.BILLING_PLANS, currentPlans, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function updateBillingPlanOptimistic(
  planId: string,
  input: BillingPlanInput,
  currentPlans: BillingPlan[],
  onError?: (error: Error) => void
) {
  const amounts = optimisticGrantFields(input);
  const hasAmount =
    input.grant_amount_nano_usd !== undefined ||
    input.grant_amount_usd !== undefined;
  const updatedPlans = currentPlans.map((p) =>
    p.id === planId
      ? {
          ...p,
          ...input,
          ...(hasAmount ? amounts : {}),
          group_ids: input.group_ids ?? p.group_ids,
          enabled: input.enabled ?? p.enabled,
        }
      : p
  );
  mutate(SWR_KEYS.BILLING_PLANS, updatedPlans, false);

  try {
    await api.updateBillingPlan(planId, input);
    mutate(SWR_KEYS.BILLING_PLANS);
    mutate(SWR_KEYS.USERS);
  } catch (error) {
    mutate(SWR_KEYS.BILLING_PLANS, currentPlans, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function deleteBillingPlanOptimistic(
  planId: string,
  currentPlans: BillingPlan[],
  onError?: (error: Error) => void
) {
  const filteredPlans = currentPlans.filter((p) => p.id !== planId);
  mutate(SWR_KEYS.BILLING_PLANS, filteredPlans, false);

  try {
    await api.deleteBillingPlan(planId);
    mutate(SWR_KEYS.BILLING_PLANS);
  } catch (error) {
    mutate(SWR_KEYS.BILLING_PLANS, currentPlans, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function resetBillingPlanOptimistic(
  plan: BillingPlan,
  onError?: (error: Error) => void
): Promise<{ success: boolean; reset_count: number }> {
  let snapshot: User[] | undefined;
  await mutate(
    SWR_KEYS.USERS,
    (current: User[] | undefined) => {
      snapshot = current;
      if (!current) return current;
      return current.map((user) => {
        if (
          user.billing_plan_id !== plan.id ||
          user.balance_unlimited ||
          !user.enabled
        ) {
          return user;
        }
        return {
          ...user,
          balance_nano_usd: plan.grant_amount_nano_usd,
          balance_usd: plan.grant_amount_usd,
        };
      });
    },
    false
  );

  try {
    const result = await api.resetBillingPlan(plan.id);
    mutate(SWR_KEYS.USERS);
    mutate(SWR_KEYS.ME);
    mutate(SWR_KEYS.STATS);
    return result;
  } catch (error) {
    mutate(SWR_KEYS.USERS, snapshot, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function createApiKeyOptimistic(
  input: CreateApiKeyInput,
  _currentKeys: ApiKey[],
  onError?: (error: Error) => void
) {
  try {
    const result = await api.createApiKey(input);
    // Revalidate to get the new key in list
    mutate(SWR_KEYS.API_KEYS);
    mutate(SWR_KEYS.STATS);
    return result;
  } catch (error) {
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function updateApiKeyOptimistic(
  keyId: string,
  input: UpdateApiKeyInput,
  currentKeys: ApiKey[],
  onError?: (error: Error) => void
) {
  // Optimistic update
  const updatedKeys = currentKeys.map((k) =>
    k.id === keyId ? { ...k, ...input } : k
  );
  mutate(SWR_KEYS.API_KEYS, updatedKeys, false);

  try {
    const result = await api.updateApiKey(keyId, input);
    // Revalidate
    mutate(SWR_KEYS.API_KEYS);
    return result;
  } catch (error) {
    // Rollback on error
    mutate(SWR_KEYS.API_KEYS, currentKeys, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function deleteApiKeyOptimistic(
  keyId: string,
  currentKeys: ApiKey[],
  onError?: (error: Error) => void
) {
  // Optimistic update
  const filteredKeys = currentKeys.filter((k) => k.id !== keyId);
  mutate(SWR_KEYS.API_KEYS, filteredKeys, false);

  try {
    await api.deleteApiKey(keyId);
    // Revalidate
    mutate(SWR_KEYS.API_KEYS);
    mutate(SWR_KEYS.STATS);
  } catch (error) {
    // Rollback on error
    mutate(SWR_KEYS.API_KEYS, currentKeys, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function batchDeleteApiKeysOptimistic(
  keyIds: string[],
  currentKeys: ApiKey[],
  onError?: (error: Error) => void
) {
  // Optimistic update
  const filteredKeys = currentKeys.filter((k) => !keyIds.includes(k.id));
  mutate(SWR_KEYS.API_KEYS, filteredKeys, false);

  try {
    await api.batchDeleteApiKeys(keyIds);
    // Revalidate
    mutate(SWR_KEYS.API_KEYS);
    mutate(SWR_KEYS.STATS);
  } catch (error) {
    // Rollback on error
    mutate(SWR_KEYS.API_KEYS, currentKeys, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

// Provider mutation helpers
export async function createProviderOptimistic(
  input: CreateProviderInput,
  _currentProviders: Provider[],
  onError?: (error: Error) => void
) {
  try {
    const result = await api.createProvider(input);
    // Revalidate to get the new provider
    mutate(SWR_KEYS.PROVIDERS);
    mutate(SWR_KEYS.STATS);
    mutate(SWR_KEYS.CONFIG);
    mutate(SWR_KEYS.MARKETPLACE_MODELS);
    return result;
  } catch (error) {
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function createWholesaleProviderOptimistic(
  input: CreateWholesaleProviderInput,
  onError?: (error: Error) => void
) {
  try {
    const result = await api.createWholesaleProvider(input);
    mutate(SWR_KEYS.PROVIDERS);
    mutate(SWR_KEYS.STATS);
    mutate(SWR_KEYS.CONFIG);
    mutate(SWR_KEYS.MARKETPLACE_MODELS);
    return result;
  } catch (error) {
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function updateProviderOptimistic(
  id: string,
  input: UpdateProviderInput,
  currentProviders: Provider[],
  onError?: (error: Error) => void
) {
  const updatedProviders = currentProviders.map((p) =>
    p.id === id ? { ...p, ...input } : p
  );
  mutate(SWR_KEYS.PROVIDERS, updatedProviders, false);

  try {
    const result = await api.updateProvider(id, input);
    mutate(providerDetailSWRKey(id), result, false);
    mutate(SWR_KEYS.PROVIDERS);
    mutate(SWR_KEYS.CONFIG);
    mutate(SWR_KEYS.MARKETPLACE_MODELS);
    return result;
  } catch (error) {
    // Revalidate from server rather than rolling back to a potentially stale snapshot
    mutate(SWR_KEYS.PROVIDERS);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function deleteProviderOptimistic(
  id: string,
  currentProviders: Provider[],
  onError?: (error: Error) => void
) {
  // Optimistic update
  const filteredProviders = currentProviders.filter((p) => p.id !== id);
  mutate(SWR_KEYS.PROVIDERS, filteredProviders, false);

  try {
    await api.deleteProvider(id);
    // Revalidate
    mutate(SWR_KEYS.PROVIDERS);
    mutate(SWR_KEYS.STATS);
    mutate(SWR_KEYS.CONFIG);
    mutate(SWR_KEYS.MARKETPLACE_MODELS);
    mutate(providerDetailSWRKey(id), undefined, false);
  } catch (error) {
    // Rollback on error
    mutate(SWR_KEYS.PROVIDERS, currentProviders, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function reorderProviders(
  groupId: string,
  providerIds: string[],
  onError?: (error: Error) => void
) {
  try {
    await api.reorderProviders(groupId, providerIds);
    mutate(SWR_KEYS.PROVIDERS);
  } catch (error) {
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function upsertModelMetadataOptimistic(
  modelId: string,
  input: UpsertModelMetadataInput,
  currentRecords: ModelMetadataRecord[],
  onError?: (error: Error) => void
) {
  const tempRecord: ModelMetadataRecord = {
    model_id: modelId,
    source: "manual",
    updated_at: new Date().toISOString(),
    ...input,
    models_dev_provider: input.models_dev_provider ?? undefined,
    mode: input.mode ?? undefined,
    input_cost_per_token_nano: input.input_cost_per_token_nano ?? undefined,
    output_cost_per_token_nano: input.output_cost_per_token_nano ?? undefined,
    cache_read_input_cost_per_token_nano: input.cache_read_input_cost_per_token_nano ?? undefined,
    cache_creation_input_cost_per_token_nano: input.cache_creation_input_cost_per_token_nano ?? undefined,
    output_cost_per_reasoning_token_nano: input.output_cost_per_reasoning_token_nano ?? undefined,
    max_input_tokens: input.max_input_tokens ?? undefined,
    max_output_tokens: input.max_output_tokens ?? undefined,
    max_tokens: input.max_tokens ?? undefined,
    // M4b: an omitted currency keeps the stored one, or defaults to CNY for a new row, which
    // is what the server does, so the optimistic row must not guess differently.
    price_currency:
      input.price_currency
      ?? currentRecords.find((record) => record.model_id === modelId)?.price_currency
      ?? "CNY",
  };
  const exists = currentRecords.some((r) => r.model_id === modelId);
  const optimistic = exists
    ? currentRecords.map((r) => (r.model_id === modelId ? { ...r, ...tempRecord } : r))
    : [...currentRecords, tempRecord];
  mutate(SWR_KEYS.MODEL_METADATA, optimistic, false);

  try {
    const result = await api.upsertModelMetadata(modelId, input);
    mutate(SWR_KEYS.MODEL_METADATA);
    // DC8a: the server response carries raw_json, which the list projection omits.
    mutate(modelMetadataDetailSWRKey(modelId), result, false);
    mutate(SWR_KEYS.MARKETPLACE_MODELS);
    mutate(SWR_KEYS.PROVIDERS);
    return result;
  } catch (error) {
    mutate(SWR_KEYS.MODEL_METADATA, currentRecords, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function deleteModelMetadataOptimistic(
  modelId: string,
  currentRecords: ModelMetadataRecord[],
  onError?: (error: Error) => void
) {
  const filtered = currentRecords.filter((r) => r.model_id !== modelId);
  mutate(SWR_KEYS.MODEL_METADATA, filtered, false);

  try {
    await api.deleteModelMetadata(modelId);
    mutate(SWR_KEYS.MODEL_METADATA);
    // DC8a: drop the detail entry without revalidation; the record no longer exists.
    mutate(modelMetadataDetailSWRKey(modelId), undefined, false);
    mutate(SWR_KEYS.MARKETPLACE_MODELS);
    mutate(SWR_KEYS.PROVIDERS);
  } catch (error) {
    mutate(SWR_KEYS.MODEL_METADATA, currentRecords, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function deletePricingProfileOptimistic(
  profile: string,
  currentProfiles: BillingRateProfileSummary[]
): Promise<DeletePricingProfileResult> {
  mutate(
    SWR_KEYS.BILLING_RATE_PROFILES,
    currentProfiles.filter((summary) => summary.pricing_profile !== profile),
    false
  );
  try {
    const result = await api.deletePricingProfile(profile);
    mutate(SWR_KEYS.BILLING_RATES);
    mutate(SWR_KEYS.BILLING_RATE_PROFILES);
    // The per-profile rate cache may hold the deleted profile's rows.
    revalidateBillingRateKeys();
    mutate(SWR_KEYS.PROVIDERS);
    return result;
  } catch (error) {
    mutate(SWR_KEYS.BILLING_RATE_PROFILES, currentProfiles, false);
    throw error;
  }
}

export async function syncModelMetadata(
  onError?: (error: Error) => void
) {
  try {
    const result = await api.syncModelMetadataFromModelsDev();
    mutate(SWR_KEYS.MODEL_METADATA);
    mutate(SWR_KEYS.BILLING_RATES);
    mutate(SWR_KEYS.BILLING_RATE_PROFILES);
    revalidateBillingRateKeys();
    // DC8: sync replaces both catalogs wholesale, so cached detail rows are stale too.
    mutate((key) => typeof key === "string" && key.startsWith("model-metadata-detail:"));
    mutate(SWR_KEYS.MARKETPLACE_MODELS);
    mutate(SWR_KEYS.PROVIDERS);
    return result;
  } catch (error) {
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function upsertBillingRateOptimistic(
  id: string,
  input: UpsertBillingRateInput,
  currentRecords: BillingRateRecord[],
  onError?: (error: Error) => void
) {
  const existing = currentRecords.find((record) => record.id === id);
  const updates = Object.fromEntries(
    Object.entries(input).filter(([, value]) => value !== undefined)
  );
  const tempRecord: BillingRateRecord = {
    id,
    pricing_profile: input.pricing_profile ?? "",
    model_pattern: input.model_pattern ?? null,
    provider_type: input.provider_type ?? null,
    rate_kind: input.rate_kind ?? "token",
    usage_class: input.usage_class ?? "",
    unit: input.unit ?? "token",
    unit_price_nano: input.unit_price_nano ?? "0",
    // MB-A2c: mirrors the server default for a newly created rate, so the optimistic row
    // shows the same currency the server will persist.
    unit_price_currency: input.unit_price_currency ?? "CNY",
    peak_unit_price_nano: input.peak_unit_price_nano ?? null,
    context_tier: input.context_tier ?? null,
    service_tier: input.service_tier ?? null,
    modality: input.modality ?? null,
    cache_ttl: input.cache_ttl ?? null,
    match_json: input.match_json ?? {},
    priority: input.priority ?? 0,
    enabled: input.enabled ?? true,
    raw_json: input.raw_json ?? {},
    ...existing,
    ...updates,
    source: input.source ?? "manual",
    updated_at: new Date().toISOString(),
  };
  const rollback = await upsertCachedBillingRate(tempRecord);

  try {
    const result = await api.upsertBillingRate(id, input);
    await upsertCachedBillingRate(result);
    mutate(SWR_KEYS.BILLING_RATES);
    mutate(SWR_KEYS.BILLING_RATE_PROFILES);
    revalidateBillingRateKeys();
    mutate(SWR_KEYS.PROVIDERS);
    return result;
  } catch (error) {
    await rollback();
    revalidateBillingRateKeys();
    mutate(SWR_KEYS.BILLING_RATES);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function deleteBillingRateOptimistic(
  id: string,
  onError?: (error: Error) => void
) {
  mutateCachedBillingRates((records) => records.filter((r) => r.id !== id));

  try {
    await api.deleteBillingRate(id);
    mutate(SWR_KEYS.BILLING_RATES);
    mutate(SWR_KEYS.BILLING_RATE_PROFILES);
    revalidateBillingRateKeys();
    mutate(SWR_KEYS.PROVIDERS);
  } catch (error) {
    revalidateBillingRateKeys();
    mutate(SWR_KEYS.BILLING_RATES);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

/**
 * Copies a pricing profile's rates under a new name (MB-A7).
 *
 * Not optimistic: the server decides how many rows exist and refuses a target that already
 * prices traffic, so guessing the result would show a profile that may not be created.
 */
export async function copyPricingProfile(
  profile: string,
  targetProfile: string,
  onError?: (error: Error) => void,
) {
  try {
    const result = await api.copyPricingProfile(profile, targetProfile);
    await mutate(SWR_KEYS.BILLING_RATES);
    mutate(SWR_KEYS.BILLING_RATE_PROFILES);
    revalidateBillingRateKeys();
    mutate(SWR_KEYS.PROVIDERS);
    return result;
  } catch (error) {
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

/**
 * Renames a model inside one pricing profile (MB-A9, UI20b).
 *
 * Not optimistic: the server decides which rows move and which synchronized rows stay under
 * the former name, so a guessed result could show a model list the database does not hold.
 */
export async function renamePricingProfileModel(
  profile: string,
  model: string,
  targetModel: string,
  onError?: (error: Error) => void,
) {
  try {
    const result = await api.renamePricingProfileModel(profile, model, targetModel);
    await mutate(SWR_KEYS.BILLING_RATES);
    mutate(SWR_KEYS.BILLING_RATE_PROFILES);
    revalidateBillingRateKeys();
    mutate(SWR_KEYS.PROVIDERS);
    return result;
  } catch (error) {
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function syncBillingRatesCatalog(
  onError?: (error: Error) => void
) {
  try {
    const result = await api.syncBillingRatesCatalog();
    mutate(SWR_KEYS.BILLING_RATES);
    mutate(SWR_KEYS.BILLING_RATE_PROFILES);
    revalidateBillingRateKeys();
    mutate(SWR_KEYS.PROVIDERS);
    return result;
  } catch (error) {
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

export async function updatePricingProfilePatternsOptimistic(
  patterns: PricingProfilePattern[],
  currentPatterns: PricingProfilePattern[],
  onError?: (error: Error) => void
) {
  mutate(SWR_KEYS.PRICING_PROFILE_PATTERNS, patterns, false);

  try {
    const result = await api.updatePricingProfilePatterns(patterns);
    mutate(SWR_KEYS.PRICING_PROFILE_PATTERNS, result.patterns, false);
    mutate(SWR_KEYS.SETTINGS);
    mutate(SWR_KEYS.PROVIDERS);
    return result.patterns;
  } catch (error) {
    mutate(SWR_KEYS.PRICING_PROFILE_PATTERNS, currentPatterns, false);
    if (onError && error instanceof Error) {
      onError(error);
    }
    throw error;
  }
}

// Global revalidation helpers
export function revalidateAll() {
  return mutate(() => true);
}

export function clearCache() {
  return mutate(
    (key) =>
      !(
        typeof key === "string" &&
        (key === SWR_KEYS.PUBLIC_SETTINGS || key.startsWith("/api/public/"))
      ),
    undefined,
    { revalidate: false }
  );
}
