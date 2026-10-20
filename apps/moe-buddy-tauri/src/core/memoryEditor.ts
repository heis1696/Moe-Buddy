import type { MemoryView, PrototypeError } from './types';
import { describeHostFailure } from './failure';

/** 正在编辑的记忆版本；保存时显式带回，不在前端自行推进业务版本。 */
export interface MemoryEditState {
  id: string;
  revision: number;
  baseText: string;
}

export interface MemoryDeleteArm {
  id: string;
  revision: number;
}

export function beginMemoryEdit(memory: MemoryView): MemoryEditState {
  return { id: memory.id, revision: memory.revision, baseText: memory.text };
}

export function memoryEditDirty(edit: MemoryEditState | null, draft: string): boolean {
  if (edit === null) return false;
  return draft !== edit.baseText;
}

/**
 * 快照里同一条记忆出现更高版本，或已从列表消失时，保存会触发版本冲突。
 * 这里只用于提示并保留草稿，不自动重试、不自动改写版本号。
 */
export function memoryEditSuperseded(
  edit: MemoryEditState | null,
  memories: readonly MemoryView[],
): boolean {
  if (edit === null) return false;
  const current = memories.find((memory) => memory.id === edit.id);
  return current === undefined || current.revision !== edit.revision;
}

export function armMemoryDelete(memory: MemoryView): MemoryDeleteArm {
  return { id: memory.id, revision: memory.revision };
}

export function isMemoryDeleteArmed(
  arm: MemoryDeleteArm | null,
  memory: MemoryView,
): boolean {
  return arm !== null && arm.id === memory.id && arm.revision === memory.revision;
}

/** 确认删除必须精确命中用户看到的那一版记忆；否则保持未执行。 */
export function confirmMemoryDelete(
  arm: MemoryDeleteArm | null,
  id: string,
  revision: number,
): boolean {
  return arm !== null && arm.id === id && arm.revision === revision;
}

export function isMemoryRevisionConflict(error: PrototypeError): boolean {
  return error.code === 'memory_revision_conflict';
}

/**
 * 保存或删除失败时保留用户草稿，并区分版本冲突与普通失败。
 * 不提供“重试”按钮；刷新列表是重新读取状态，不是自动重发修改。
 */
export function memorySaveFailureText(error: PrototypeError): string {
  if (isMemoryRevisionConflict(error)) {
    return '这条记忆的版本已变化或已不可用；本次修改未保存，编辑草稿已保留。请刷新列表确认最新版本后再修改。';
  }
  return describeHostFailure('memory_update', error);
}

export function memoryDeleteFailureText(error: PrototypeError): string {
  if (isMemoryRevisionConflict(error)) {
    return '这条记忆的版本已变化或已不可用；本次删除未执行。请刷新列表后重新确认。';
  }
  return describeHostFailure('memory_delete', error);
}

/**
 * 保存失败时冻结当前编辑目标与草稿，调用方据此原样恢复表单；
 * 不推进 revision，也不把失败当作已保存。
 */
export interface MemoryDraftRetention {
  edit: MemoryEditState;
  draft: string;
  message: string;
  conflict: boolean;
}

export function retainMemoryDraft(
  edit: MemoryEditState,
  draft: string,
  error: PrototypeError,
): MemoryDraftRetention {
  return {
    edit: { ...edit },
    draft,
    message: memorySaveFailureText(error),
    conflict: isMemoryRevisionConflict(error),
  };
}
