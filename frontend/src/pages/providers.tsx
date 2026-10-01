import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { AlertTriangle, Plus, Server } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
	AlertDialog,
	AlertDialogAction,
	AlertDialogCancel,
	AlertDialogContent,
	AlertDialogDescription,
	AlertDialogFooter,
	AlertDialogHeader,
	AlertDialogTitle
} from '@/components/ui/alert-dialog'
import { toast } from 'sonner'
import type { AccountClass, Provider } from '@/lib/api'
import {
	useProviders,
	useModelMetadata,
	useSettings,
	useTransformRegistry,
	deleteProviderOptimistic,
	updateProviderOptimistic,
	reorderProviders,
	useDashboardGroups
} from '@/lib/swr'
import { AnimatedButton, PageWrapper, motion, transitions } from '@/components/ui/motion'
import { EmptyState } from '@/components/ui/empty-state'
import { PageHeader } from '@/components/ui/page-header'
import { CardsPageSkeleton } from '@/components/ui/page-skeleton'
import { ProviderCard } from './providers/ProviderCard'
import { ProviderDialog } from './providers/ProviderDialog'
import { WholesaleProviderDialog } from './providers/WholesaleProviderDialog'
import { DEFAULT_REASONING_SUFFIX_MAP } from './providers/shared'

const ACCOUNT_CLASSES = ['standard', 'enterprise', 'private', 'agent'] as const

export function ProvidersPage() {
	const { t } = useTranslation()
	const { data: providersData, error: providersError, isLoading, mutate: reloadProviders } = useProviders()
	const providers = useMemo(() => providersData ?? [], [providersData])
	const { data: groups = [], error: groupsError, isLoading: groupsLoading, mutate: reloadGroups } = useDashboardGroups()
	const [accountClass, setAccountClass] = useState<AccountClass>("standard")
	const groupById = useMemo(() => new Map(groups.map(group => [group.id, group])), [groups])
	const visibleProviders = useMemo(
		() => providers.filter(provider => groupById.get(provider.group_id)?.account_class === accountClass),
		[accountClass, groupById, providers]
	)
	const { data: settings, error: settingsError, isLoading: settingsLoading, mutate: reloadSettings } = useSettings()
	const { data: transformRegistry = [], isLoading: transformRegistryLoading, error: transformsError, mutate: reloadTransforms } =
		useTransformRegistry()
	const { data: modelMetadata = [], error: metadataError, isLoading: metadataLoading, mutate: reloadMetadata } = useModelMetadata()
	const loadError = providersError || groupsError || settingsError || transformsError || metadataError
	const reasoningSuffixMap =
		settings?.reasoning_suffix_map ?? DEFAULT_REASONING_SUFFIX_MAP
	const [createOpen, setCreateOpen] = useState(false)
	const [wholesaleOpen, setWholesaleOpen] = useState(false)
	const [editProvider, setEditProvider] = useState<Provider | null>(null)
	const [deleteTarget, setDeleteTarget] = useState<Provider | null>(null)
	const [draggingProviderId, setDraggingProviderId] = useState<string | null>(null)

	// PP-WF1: the agent scope creates Providers through the wholesale flow, so its create
	// action must not open the ordinary editor.
	const openCreate = () => {
		if (accountClass === 'agent') {
			setWholesaleOpen(true)
		} else {
			setCreateOpen(true)
		}
	}

	const applyReorder = async (groupId: string, orderedIds: string[]) => {
		try {
			await reorderProviders(groupId, orderedIds)
			toast.success(t('providers.reorderSuccess'))
		} catch (error) {
			toast.error(error instanceof Error ? error.message : t('common.error'))
		}
	}

	const moveProvider = async (from: number, to: number) => {
		if (to < 0 || to >= visibleProviders.length || from === to) {
			return
		}
		const groupId = visibleProviders[from]?.group_id
		if (!groupId || visibleProviders[to]?.group_id !== groupId) return
		const next = [...visibleProviders]
		const [item] = next.splice(from, 1)
		next.splice(to, 0, item)
		await applyReorder(
			groupId,
			next.filter(provider => provider.group_id === groupId).map(provider => provider.id)
		)
	}

	const handleDrop = async (targetProviderId: string) => {
		if (!draggingProviderId || draggingProviderId === targetProviderId) {
			return
		}
		const next = [...visibleProviders]
		const from = next.findIndex(provider => provider.id === draggingProviderId)
		const to = next.findIndex(provider => provider.id === targetProviderId)
		if (from < 0 || to < 0) {
			return
		}
		const groupId = next[from]?.group_id
		if (!groupId || next[to]?.group_id !== groupId) {
			setDraggingProviderId(null)
			return
		}
		const [item] = next.splice(from, 1)
		next.splice(to, 0, item)
		setDraggingProviderId(null)
		await applyReorder(
			groupId,
			next.filter(provider => provider.group_id === groupId).map(provider => provider.id)
		)
	}

	const handleDelete = async (provider: Provider) => {
		setDeleteTarget(provider)
	}

	const confirmDelete = async () => {
		if (!deleteTarget) return
		try {
			await deleteProviderOptimistic(deleteTarget.id, providers)
			toast.success(t('providers.deleteSuccess'))
		} catch (error) {
			toast.error(error instanceof Error ? error.message : t('common.error'))
		} finally {
			setDeleteTarget(null)
		}
	}

	const handleToggle = async (provider: Provider, enabled: boolean) => {
		try {
			await updateProviderOptimistic(provider.id, { enabled }, providers)
			toast.success(t('providers.updateSuccess'))
		} catch (error) {
			toast.error(error instanceof Error ? error.message : t('common.error'))
		}
	}

	if (!loadError && (isLoading || groupsLoading || settingsLoading || transformRegistryLoading || metadataLoading)) {
		return (
			<PageWrapper className='space-y-6'>
				<CardsPageSkeleton />
			</PageWrapper>
		)
	}

	if (loadError) {
		const message = loadError instanceof Error ? loadError.message : t('common.error')
		return (
			<PageWrapper className='space-y-6'>
				<motion.div
					initial={{ opacity: 0, y: -10 }}
					animate={{ opacity: 1, y: 0 }}
					transition={transitions.normal}
				>
					<PageHeader title={t('providers.title')} description={t('providers.description')} />
				</motion.div>
				<EmptyState
					variant='card'
					role='alert'
					icon={<AlertTriangle className='h-12 w-12 text-destructive' />}
					title={t('providers.loadFailed', { defaultValue: 'Failed to load providers' })}
					description={<span className='font-mono text-xs break-all'>{message}</span>}
					action={<Button variant='outline' onClick={() => {
						void reloadProviders()
						void reloadGroups()
						void reloadSettings()
						void reloadTransforms()
						void reloadMetadata()
					}}>{t('common.retry', { defaultValue: 'Retry' })}</Button>}
				/>
			</PageWrapper>
		)
	}

	return (
		<PageWrapper className='space-y-6'>
			<motion.div
				initial={{ opacity: 0, y: -10 }}
				animate={{ opacity: 1, y: 0 }}
				transition={transitions.normal}
			>
				<PageHeader title={t('providers.title')} description={t('providers.description')} actions={(
					<AnimatedButton>
						<Button onClick={openCreate}>
							<Plus className='h-4 w-4 mr-2' />
							{accountClass === 'agent' ? t('providers.addWholesaleProvider') : t('providers.addProvider')}
						</Button>
					</AnimatedButton>
				)} />
			</motion.div>

			<div className='flex flex-col gap-2 rounded-xl border bg-card p-3 sm:flex-row sm:items-center sm:justify-between'>
				<div><p className='text-sm font-medium'>{t('providers.accountClass')}</p><p className='text-xs text-muted-foreground'>{t('providers.accountClassDescription')}</p></div>
				<Tabs value={accountClass} onValueChange={value => setAccountClass(value as AccountClass)}>
						<TabsList className='grid w-full grid-cols-4 rounded-lg sm:w-[32rem]'>
							{ACCOUNT_CLASSES.map(value => <TabsTrigger key={value} value={value}>{t(`accountClass.${value}`)}</TabsTrigger>)}
						</TabsList>
					</Tabs>
			</div>

			<div className='space-y-4'>
				{visibleProviders.length === 0 && (
					<motion.div
						initial={{ opacity: 0, scale: 0.95 }}
						animate={{ opacity: 1, scale: 1 }}
						transition={transitions.normal}
					>
						<EmptyState
							variant='card'
							icon={<Server className='h-12 w-12' />}
							title={t('providers.noProviders')}
							description={t('providers.emptyStateDesc')}
							action={<Button variant='outline' onClick={openCreate}><Plus className='h-4 w-4 mr-2' />{accountClass === 'agent' ? t('providers.addWholesaleProvider') : t('providers.addProvider')}</Button>}
						/>
					</motion.div>
				)}

				{visibleProviders.map((provider, index) => (
					<ProviderCard
						key={provider.id}
						provider={provider}
						index={index}
						total={visibleProviders.length}
						onEdit={setEditProvider}
						onDelete={handleDelete}
						onMove={moveProvider}
						onToggle={handleToggle}
						onDragStart={setDraggingProviderId}
						onDrop={handleDrop}
						modelMetadata={modelMetadata}
					/>
				))}
			</div>

			<ProviderDialog
				open={createOpen}
				onOpenChange={setCreateOpen}
				mode='create'
				current={null}
				providers={providers}
				transformRegistry={transformRegistry}
				transformRegistryLoading={transformRegistryLoading}
				modelMetadata={modelMetadata}
				reasoningSuffixMap={reasoningSuffixMap}
				settings={settings}
				accountClass={accountClass}
			/>

			<ProviderDialog
				open={!!editProvider}
				onOpenChange={open => {
					if (!open) {
						setEditProvider(null)
					}
				}}
				mode='edit'
				current={editProvider}
				providers={providers}
				transformRegistry={transformRegistry}
				transformRegistryLoading={transformRegistryLoading}
				modelMetadata={modelMetadata}
				reasoningSuffixMap={reasoningSuffixMap}
				settings={settings}
				accountClass={accountClass}
			/>

			<WholesaleProviderDialog
				open={wholesaleOpen}
				onOpenChange={setWholesaleOpen}
			/>

			<AlertDialog open={!!deleteTarget} onOpenChange={open => { if (!open) setDeleteTarget(null) }}>
				<AlertDialogContent>
					<AlertDialogHeader>
						<AlertDialogTitle>{t('providers.deleteConfirm')}</AlertDialogTitle>
						<AlertDialogDescription>
							{t('providers.deleteConfirmDesc', { id: deleteTarget?.name })}
						</AlertDialogDescription>
					</AlertDialogHeader>
					<AlertDialogFooter>
						<AlertDialogCancel>{t('common.cancel')}</AlertDialogCancel>
						<AlertDialogAction
							className='bg-destructive text-destructive-foreground hover:bg-destructive/90'
							onClick={confirmDelete}
						>
							{t('common.delete')}
						</AlertDialogAction>
					</AlertDialogFooter>
				</AlertDialogContent>
			</AlertDialog>
		</PageWrapper>
	)
}
