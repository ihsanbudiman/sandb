// "Review with AI" on a query plan the user ran in the editor. The assistant may not run EXPLAIN ANALYZE
// (it executes the statement), so the user runs it and sends the result as a chat question.
import type { Cell, Engine, QueryResult } from './api';
import { chat, current, openChatOnTab, send, skillsOf } from './chat.svelte';
import { notify } from './notice.svelte';
import { cellText, isCutOff } from './results';

/** Most plan characters in one question; a longer plan is cut, and the question says so. */
const MAX_PLAN_CHARS = 24_000;

const TUNING_SKILL: Record<Engine, string> = { pg: 'postgres-tuning', mysql: 'mysql-tuning', sqlite: 'sqlite-tuning', mongo: 'mongo-tuning' };

const withoutComments = (sql: string) => sql.replace(/--.*$/gm, '').replace(/\/\*[\s\S]*?\*\//g, '').trim();

/** The result is the output of EXPLAIN (SQL) or of an `explain` command (MongoDB). */
export function isPlan(r: QueryResult | null | undefined, engine: Engine): boolean {
  if (!r?.sql || r.error || r.note || !r.rows.length) return false;
  return engine === 'mongo' ? /\bexplain\b/.test(r.sql) : /^explain\b/i.test(withoutComments(r.sql));
}

/** The plan as text: MongoDB's documents as JSON; one plan column as its lines; more columns as a tab-separated table. */
function planText(r: QueryResult): string {
  if (r.documents?.length) return JSON.stringify(r.documents.length === 1 ? r.documents[0] : r.documents, null, 2);
  const cell = (v: Cell) => (v === null ? 'NULL' : cellText(v));
  const lines = r.rows.map((row) => r.columns.map((c) => cell(row[c])).join('\t'));
  return r.columns.length === 1 ? lines.join('\n') : [r.columns.join('\t'), ...lines].join('\n');
}

/** Opens the assistant on the tab's database and asks it to review the plan, with the tuning skill of the engine. */
export function reviewPlan(r: QueryResult, engine: Engine) {
  if (chat.live) {
    notify(false, 'The assistant is still answering. Try again when it is done.');
    return;
  }
  openChatOnTab();
  let plan = planText(r);
  const notes: string[] = [];
  if (plan.length > MAX_PLAN_CHARS) {
    plan = plan.slice(0, MAX_PLAN_CHARS);
    notes.push(`The plan was cut after ${MAX_PLAN_CHARS.toLocaleString()} characters.`);
  }
  if (r.truncated) notes.push(`The editor showed only the first ${r.rows.length} rows of the plan.`);
  if (r.rows.some((row) => Object.values(row).some(isCutOff))) notes.push('Some plan cells were cut at the preview size.');
  const lang = engine === 'mongo' ? 'javascript' : 'sql';
  const text = [
    `Review the performance of this ${engine === 'mongo' ? 'query' : 'statement'}. I ran it in the editor (${r.ms} ms); do not run it again. Explain the slow parts of the plan and what to change.`,
    `\`\`\`${lang}\n${r.sql}\n\`\`\``,
    `Plan:\n\`\`\`${engine === 'mongo' ? 'json' : 'text'}\n${plan}\n\`\`\``,
    ...notes,
  ].join('\n\n');
  void send(text, [...skillsOf(current()), TUNING_SKILL[engine]]);
}
