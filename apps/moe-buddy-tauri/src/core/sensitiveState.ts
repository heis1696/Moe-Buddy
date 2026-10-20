import type { MemoryEditState } from './memoryEditor';

/**
 * 只存在于当前窗口内存中的面板草稿。撤权、显式放弃编辑或窗口卸载时清空；
 * 不写 localStorage，也不随快照回显 API Key。
 */
export interface SensitiveDrafts {
  apiKeyInput: string;
  clearApiKey: boolean;
  memoryEdit: MemoryEditState | null;
  rememberDraft: string;
  searchQuery: string;
}

export function clearedSensitiveDrafts(): SensitiveDrafts {
  return {
    apiKeyInput: '',
    clearApiKey: false,
    memoryEdit: null,
    rememberDraft: '',
    searchQuery: '',
  };
}

export function hasSensitiveDrafts(drafts: SensitiveDrafts): boolean {
  return (
    drafts.apiKeyInput.length > 0 ||
    drafts.clearApiKey ||
    drafts.memoryEdit !== null ||
    drafts.rememberDraft.length > 0 ||
    drafts.searchQuery.trim().length > 0
  );
}
