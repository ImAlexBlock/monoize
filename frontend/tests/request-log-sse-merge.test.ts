import { describe, expect, test } from 'bun:test'
import type { RequestLog } from '../src/lib/api'
import { mergeSSELogs } from '../src/pages/request-logs/merge-sse-logs'

function requestLog(overrides: Partial<RequestLog> = {}): RequestLog {
	return {
		id: 'request-1',
		request_id: 'request-1',
		created_at: '2026-10-03T00:00:00.000Z',
		status: 'pending',
		is_stream: true,
		model: 'test-model',
		provider: {},
		channel: {},
		user: { id: 'user-1' },
		api_key: {},
		tokens: {},
		timing: {},
		billing: {},
		error: {},
		...overrides
	}
}

const allRows = () => true

describe('request-log SSE merging (FL56a, FL56b)', () => {
	test('applies live timing and terminal snapshots with the same id in one buffered flush', () => {
		const pending = requestLog({ timing: { ttfb_ms: null } })
		const timed = requestLog({ timing: { ttfb_ms: 120 } })
		const terminal = requestLog({
			status: 'success',
			timing: { ttfb_ms: 120, duration_ms: 900 },
			tokens: { output: 30 }
		})

		expect(mergeSSELogs([], [pending, timed], allRows)).toEqual([timed])
		expect(mergeSSELogs([], [pending, timed, terminal], allRows)).toEqual([terminal])
		expect(mergeSSELogs([pending], [timed, terminal], allRows)).toEqual([terminal])
	})

	test('removes a pending row when its same-id terminal snapshot fails the status filter', () => {
		const pending = requestLog()
		const timed = requestLog({ timing: { ttfb_ms: 0 } })
		const terminal = requestLog({ status: 'success', timing: { ttfb_ms: 0 } })
		const pendingOnly = (log: RequestLog) => log.status === 'pending'

		expect(mergeSSELogs([pending], [timed], pendingOnly)).toEqual([timed])
		expect(mergeSSELogs([pending], [timed, terminal], pendingOnly)).toEqual([])
	})

	test('allows one request to enter, exit, and reenter the active filter in the same merge', () => {
		const excluded = requestLog({ model: 'other-model' })
		const included = requestLog()
		const reentered = requestLog({ timing: { ttfb_ms: 321 } })
		const selectedModel = (log: RequestLog) => log.model === 'test-model'

		expect(mergeSSELogs([], [excluded, included], selectedModel)).toEqual([included])
		expect(mergeSSELogs([], [excluded, included, excluded], selectedModel)).toEqual([])
		expect(
			mergeSSELogs([], [excluded, included, excluded, reentered], selectedModel)
		).toEqual([reentered])
	})

	test('replaces a persisted row by request_id even when the SSE row has a different id', () => {
		const persisted = requestLog({ id: 'database-row-1' })
		const terminal = requestLog({ status: 'success', timing: { ttfb_ms: 10 } })

		expect(mergeSSELogs([persisted], [terminal, terminal], allRows)).toEqual([terminal])
	})

	test('uses id for rows without request_id and retains the last duplicate snapshot', () => {
		const original = requestLog({ request_id: undefined })
		const updated = requestLog({ request_id: undefined, timing: { ttfb_ms: 45 } })

		expect(mergeSSELogs([original], [original, updated, updated], allRows)).toEqual([
			updated
		])
	})

	test('updates repeated snapshots in place and preserves unaffected row order', () => {
		const older = requestLog({ id: 'older', request_id: 'older' })
		const oldest = requestLog({ id: 'oldest', request_id: 'oldest' })
		const pending = requestLog()
		const timed = requestLog({ timing: { ttfb_ms: 12 } })
		const terminal = requestLog({ status: 'success', timing: { ttfb_ms: 12 } })
		const other = requestLog({ id: 'other', request_id: 'other' })
		const current = [older, pending, oldest]
		const incoming = [timed, other, terminal, other]

		expect(mergeSSELogs(current, incoming, allRows)).toEqual([
			other,
			terminal,
			older,
			oldest
		])
		expect(current).toEqual([older, pending, oldest])
		expect(incoming).toEqual([timed, other, terminal, other])
	})

	test('prepends a restored row after a filter exit without moving unrelated rows', () => {
		const older = requestLog({ id: 'older', request_id: 'older' })
		const included = requestLog()
		const excluded = requestLog({ model: 'other-model' })
		const other = requestLog({ id: 'other', request_id: 'other' })
		const restored = requestLog({ timing: { ttfb_ms: 65 } })

		expect(
			mergeSSELogs(
				[older],
				[included, other, excluded, restored],
				log => log.model === 'test-model'
			)
		).toEqual([restored, other, older])
	})
})
