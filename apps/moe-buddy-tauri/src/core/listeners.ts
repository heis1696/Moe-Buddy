import type { PrototypeError } from './types';

/** 宿主赠予的 unlisten；同步抛错或返回 rejected Promise 都必须被观察到。 */
export type Unlisten = () => void | Promise<void>;

/** 解除失败时只暴露固定安全错误，不携带宿主或运行时原始正文。 */
export const LISTENER_RELEASE_ERROR: PrototypeError = {
  code: 'listener_release_failed',
  message: '无法解除桌面事件监听，请重启 Moe Buddy。',
  disposition: 'unknown',
};

export type ReleaseOutcome =
  | { ok: true }
  | { ok: false; error: PrototypeError };

/** 使用宿主授予的 unlisten 解除监听；失败收敛为固定安全错误，不吞掉。 */
export async function releaseListener(unlisten: Unlisten): Promise<ReleaseOutcome> {
  try {
    await unlisten();
    return { ok: true };
  } catch {
    return { ok: false, error: { ...LISTENER_RELEASE_ERROR } };
  }
}

/**
 * 逐个释放监听。单个失败不阻止其余释放，但整体结果保留失败，
 * 调用方必须在重连前确认结果，不能伪装成已释放。
 */
export async function releaseListeners(
  unlisteners: readonly Unlisten[],
): Promise<ReleaseOutcome> {
  let failure: PrototypeError | null = null;
  for (const unlisten of unlisteners) {
    const outcome = await releaseListener(unlisten);
    if (!outcome.ok) failure ??= outcome.error;
  }
  return failure === null ? { ok: true } : { ok: false, error: failure };
}
