import type { RequestLog } from '@/lib/api'

function requestLogIdentity(log: RequestLog): string {
	return log.request_id ? `request:${log.request_id}` : `id:${log.id}`
}

export function mergeSSELogs(
	current: RequestLog[],
	incoming: RequestLog[],
	matchesFilters: (log: RequestLog) => boolean
): RequestLog[] {
	const next = [...current]
	const handledIdentities = new Set<string>()

	for (const log of incoming) {
		const identity = requestLogIdentity(log)
		const existingIndex = next.findIndex(item => requestLogIdentity(item) === identity)

		if (!matchesFilters(log)) {
			if (existingIndex >= 0) next.splice(existingIndex, 1)
		} else if (existingIndex >= 0 && handledIdentities.has(identity)) {
			next[existingIndex] = log
		} else {
			if (existingIndex >= 0) next.splice(existingIndex, 1)
			next.unshift(log)
		}

		handledIdentities.add(identity)
	}

	return next
}
