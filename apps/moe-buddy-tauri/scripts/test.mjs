import { spawnSync } from 'node:child_process';
import { readdirSync, rmSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// 核心模块编译成 CommonJS 后交给 node:test；生命周期回归另执行真实组件脚本。
// 不引入额外测试依赖；DOM 与操作系统窗口由 native smoke 验证。
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const tscBin = path.join(root, 'node_modules', 'typescript', 'bin', 'tsc');

rmSync(path.join(root, '.test-build'), { recursive: true, force: true });

const compile = spawnSync(process.execPath, [tscBin, '-p', 'tsconfig.test.json'], {
  cwd: root,
  stdio: 'inherit',
});
if (compile.error) {
  throw compile.error;
}
if (compile.status !== 0) {
  process.exit(compile.status ?? 1);
}

const testFiles = readdirSync(path.join(root, 'tests'))
  .filter((name) => name.endsWith('.test.cjs'))
  .sort()
  .map((name) => path.join(root, 'tests', name));

const tests = spawnSync(process.execPath, ['--test', ...testFiles], {
  cwd: root,
  stdio: 'inherit',
});
if (tests.error) {
  throw tests.error;
}
process.exit(tests.status ?? 1);
