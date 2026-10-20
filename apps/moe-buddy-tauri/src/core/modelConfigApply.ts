import { modelFormAfterApply, type ModelFormState } from './modelConfig';
import { displayModelSettings } from './projection';
import type {
  PrototypeSnapshot,
  SnapshotResponse,
} from './types';
import type { SnapshotUpdate } from './snapshot';

export interface ModelSettingsApplyFlow {
  dispatch: () => Promise<SnapshotResponse>;
  applyResponse: (response: SnapshotResponse) => SnapshotUpdate;
  currentSnapshot: () => PrototypeSnapshot | null;
  componentIsActive: () => boolean;
  interactionIsCurrent: () => boolean;
  /** 未配置模型时，活动档案已绑定的人设仍是表单基线。 */
  activePersona?: () => string | null;
  onApplied: (form: ModelFormState) => void;
}

/** Apply the host result, then refill from the latest still-authorized snapshot. */
export async function completeModelSettingsApply(
  flow: ModelSettingsApplyFlow,
): Promise<SnapshotUpdate | null> {
  const response = await flow.dispatch();
  if (!flow.componentIsActive()) return null;

  const update = flow.applyResponse(response);
  if (
    update.kind !== 'success' ||
    !flow.componentIsActive() ||
    !flow.interactionIsCurrent()
  ) {
    return update;
  }

  const current = flow.currentSnapshot();
  if (current === null || current.revoked) return update;

  flow.onApplied(
    modelFormAfterApply(displayModelSettings(current), flow.activePersona?.() ?? null),
  );
  return update;
}
