import type { ToolKind } from './api';
import type { ToolPart } from './chatModel';
import { connOf } from './connections.svelte';

/** Title of each step kind in the transcript. A step of kind `other` shows its tool name. */
export const TOOL_LABEL: Record<string, string> = {
  list: 'List tables',
  describe: 'Describe table',
  read: 'Read query',
  chart: 'Chart',
  write: 'Proposed write',
  skill: 'Skill',
};

export const isReadTool = (p: ToolPart) => p.tool === 'read';
export const isWriteTool = (p: ToolPart) => p.tool === 'write';
export const isChartTool = (p: ToolPart) => p.tool === 'chart';
/** The table name of a describe step is its `sql` field. */
export const isDescribeTool = (p: ToolPart) => p.tool === 'describe';

/** True when the step ran on a MongoDB source, so its text is mongosh, not SQL. */
export const mongoStep = (p: ToolPart) => !!p.source && connOf(p.source)?.engine === 'mongo';

export const redisStep = (p: ToolPart) => p.name === 'run_redis_read' || p.name === 'render_redis_chart' || p.name === 'propose_redis_write' || (!!p.source && connOf(p.source)?.engine === 'redis');

/** What the model prepares while the arguments of a tool call still stream. */
export function startingLabel(kind: ToolKind | undefined, name: string): string {
  switch (kind) {
    case 'read':
      return 'Writing query…';
    case 'list':
    case 'describe':
      return 'Reading schema…';
    case 'chart':
      return 'Preparing chart…';
    case 'write':
      return 'Preparing change…';
    case 'skill':
      return 'Loading skill…';
    default:
      return `Using ${name}…`;
  }
}

export function destructive(p: ToolPart): boolean {
  const sql = p.sql ?? '';
  if (mongoStep(p)) return /\.\s*(deleteMany|deleteOne|findOneAndDelete|dropIndexes|dropIndex|dropDatabase|drop|remove)\s*\(/.test(sql);
  if (redisStep(p)) {
    const command = p.info?.statements[0]?.kind ?? sql.trim().split(/\s+/, 1)[0].toUpperCase();
    return ['DEL', 'UNLINK', 'FLUSHDB', 'FLUSHALL'].includes(command) ||
      (p.info?.warnings.some((w) => w.code === 'flush' || w.code === 'server_admin' || w.code === 'unknown_command') ?? false);
  }
  return /^\s*(delete|drop|truncate)/i.test(sql);
}

/** The write's method for the preview badge: the guard's statement kind when the preview carried it, else the last method call of the mongosh text. */
export function mongoTag(p: ToolPart): string {
  const kind = p.info?.statements[0]?.kind;
  if (kind) return kind;
  const calls = [...(p.sql ?? '').matchAll(/\.\s*([A-Za-z_$][\w$]*)\s*\(/g)].map((m) => m[1]);
  return calls.at(-1) ?? (p.sql ?? '').trim().split(/\s+/)[0]?.toUpperCase() ?? '';
}

export const STATUS_TEXT: Record<string, string> = { running: 'RUNNING', done: 'OK', blocked: 'BLOCKED', awaiting: 'NEEDS APPROVAL', approved: 'APPROVED', denied: 'DENIED', failed: 'FAILED' };
export const STATUS_TONE: Record<ToolPart['status'], string> = {
  running: 'text-accent',
  done: 'text-ok',
  blocked: 'text-danger',
  awaiting: 'text-warn',
  approved: 'text-ok',
  denied: 'text-danger',
  failed: 'text-danger',
};

export const STATUS_LABEL: Record<string, string> = {
  running: 'The step is running',
  done: 'The step finished',
  blocked: 'The guard rejected the step',
  awaiting: 'The write needs your approval',
  approved: 'The write was approved and ran',
  denied: 'The write was denied',
  failed: 'The step failed with an error',
};
