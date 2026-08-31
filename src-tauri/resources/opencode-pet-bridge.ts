import type { Plugin } from "@opencode-ai/plugin"

const ENDPOINT = "http://127.0.0.1:7878/event"
const THROTTLE_MS = 2000
const lastSent = new Map<string, { kind: string; at: number }>()

function extractSessionID(props: any): string {
	return (
		props?.sessionID ??
		props?.info?.sessionID ??
		props?.info?.id ??
		props?.part?.sessionID ??
		props?.session?.id ??
		props?.message?.sessionID ??
		"opencode-default"
	)
}

function questionMessage(props: any): string {
	const first = props?.questions?.[0]
	return String(first?.header ?? first?.question ?? "Agent 正在等你回答").slice(0, 80)
}

async function report(sessionID: string, kind: string, message = "") {
	const now = Date.now()
	const prev = lastSent.get(sessionID)
	if (prev && prev.kind === kind && now - prev.at < THROTTLE_MS) return
	lastSent.set(sessionID, { kind, at: now })
	try {
		await fetch(ENDPOINT, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({
				source: "opencode",
				kind,
				session_id: sessionID,
				message,
			}),
			signal: AbortSignal.timeout(1500),
		})
	} catch {}
}

const GoldenPetPlugin: Plugin = async () => {
	return {
		event: async ({ event }) => {
			const type = (event as any).type as string
			const props = (event as any).properties ?? (event as any).data
			const sid = extractSessionID(props)
			switch (type) {
				case "session.execution.started":
					await report(sid, "user_activity")
					break
				case "session.execution.succeeded":
				case "session.execution.interrupted":
					await report(sid, "done")
					break
				case "session.execution.failed":
					await report(sid, "error", String(props?.error?.message ?? props?.error ?? ""))
					break
				case "session.status":
					if (props?.status?.type === "busy") {
						await report(sid, "working")
					} else if (props?.status?.type === "idle") {
						await report(sid, "done")
					} else if (props?.status?.type === "retry") {
						await report(sid, "working", String(props?.status?.message ?? "正在重试"))
					}
					break
				case "message.updated":
				case "message.part.updated":
					if (props?.info?.role === "user") {
						await report(sid, "user_activity")
					} else if (props?.info?.role === "assistant" || !props?.info) {
						await report(sid, "working")
					}
					break
				case "permission.ask":
				case "permission.asked":
				case "permission.v2.asked":
					await report(sid, "waiting", props?.toolCall?.tool ?? "需要授权")
					break
				case "permission.respond":
				case "permission.replied":
				case "permission.v2.replied":
					await report(sid, "working")
					break
				case "question.asked":
				case "question.v2.asked":
					await report(sid, "waiting", questionMessage(props))
					break
				case "question.replied":
				case "question.rejected":
				case "question.v2.replied":
				case "question.v2.rejected":
					await report(sid, "working")
					break
				case "session.idle":
					await report(sid, "done")
					break
				case "session.error":
					await report(sid, "error", String(props?.error ?? "").slice(0, 80))
					break
				case "session.deleted":
					await report(sid, "session_end")
					break
			}
		},
	}
}

export default GoldenPetPlugin
