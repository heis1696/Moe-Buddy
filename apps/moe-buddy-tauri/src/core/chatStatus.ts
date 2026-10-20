/**
 * 只有宿主明确处于可再次发送的状态时才允许发送。
 * Sending 表示上一轮仍在进行；Unknown 表示结果未确认；其他未识别状态一律按禁止处理，
 * 避免在副作用命令结果不明时因乐观放行而重复发送。
 */
const SENDABLE_STATUSES: ReadonlySet<string> = new Set(['ready', 'failed']);

export function chatStatusAllowsSend(status: string | null | undefined): boolean {
  if (status === null || status === undefined) return false;
  return SENDABLE_STATUSES.has(status.trim().toLowerCase());
}
