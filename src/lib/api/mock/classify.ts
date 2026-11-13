import type { Engine, GuardCheck, WriteInfo } from '../generated/wire';

// The mock's only statement classifier. It stands in for the real guard and classifier with SQL keywords;
// it is not a security boundary and deliberately does not restate their rules.
const bare = (sql: string) => sql.replace(/--.*$/gm, '').replace(/\/\*[\s\S]*?\*\//g, '').trim();
const firstWord = (sql: string) => bare(sql).split(/\s+/)[0]?.toLowerCase() ?? '';
const READ: Record<string, true> = { select: true, with: true, show: true, explain: true, describe: true };
/** The SQL statements that change what the catalog lists. */
const STRUCTURE = /^(create|drop|alter|rename|truncate)$/;
/** The aggregate stages that write: the backend runs such an aggregate as a write. */
const MONGO_OUT = /\$out\b|\$merge\b/;
/** The mongosh calls that change what the catalog lists. */
const MONGO_STRUCTURE = /\.(?:drop|createIndex|createIndexes|dropIndex|dropIndexes|renameCollection)\(/;

export function statementKind(sql: string): 'read' | 'write' | 'empty' {
  const first = firstWord(sql);
  if (!first) return 'empty';
  return READ[first] ? 'read' : 'write';
}

/** MongoDB read methods; the mock runs `find` of these only. */
const MONGO_READ = /^(find|findOne|aggregate|countDocuments|estimatedDocumentCount|distinct|getIndexes|stats)$/;

function mongoMethod(part: string): { coll?: string; method: string } | null {
  const m = /^db\s*\.\s*(?:getCollection\(\s*("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')\s*\)|([A-Za-z_$][\w$]*))\s*\.\s*([A-Za-z_$][\w$]*)/.exec(part.trim());
  if (!m) return null;
  return { coll: m[2] ?? m[1].slice(1, -1).replace(/\\(["'\\])/g, '$1'), method: m[3] };
}

function statements(engine: Engine, sql: string): string[] {
  return engine === 'mongo'
    ? sql.split(';').map((p) => p.trim()).filter((p) => p.replace(/\/\/.*$/gm, '').trim())
    : sql.split(';').map((p) => p.trim()).filter((p) => statementKind(p) !== 'empty');
}

export function guard(engine: Engine, sql: string): GuardCheck {
  const parts = statements(engine, sql);
  if (parts.length !== 1) return { allowed: false, reason: parts.length ? `Exactly one statement is allowed, found ${parts.length}` : 'The query is empty' };
  if (engine === 'mongo') {
    const m = mongoMethod(parts[0]);
    if (!m) return { allowed: false, reason: 'Not a collection read (mock guard)', kind: firstWord(parts[0]) };
    if (!MONGO_READ.test(m.method) || MONGO_OUT.test(parts[0])) return { allowed: false, reason: `${m.method} is not a read (mock guard)`, kind: m.method };
    return { allowed: true, kind: m.method };
  }
  const first = firstWord(parts[0]);
  if (statementKind(parts[0]) !== 'read') return { allowed: false, reason: `${first.toUpperCase()} is not a read (mock guard)`, kind: first.toUpperCase() };
  return { allowed: true, kind: first === 'with' ? 'SELECT' : first.toUpperCase() };
}

export function classify(engine: Engine, sql: string, database?: string | null): WriteInfo {
  const parts = statements(engine, sql);
  const info: WriteInfo = { statements: [], warnings: [] };
  if (parts.length > 1) info.warnings.push({ code: 'multiple_statements', message: `${parts.length} statements will run` });
  let db = database ?? '';
  for (const part of parts) {
    if (engine === 'mongo') {
      // `use` changes the database for the statements after it, as the run does.
      const use = /^use\s+([^\s;]+)/.exec(part);
      if (use) {
        db = use[1];
        info.statements.push({ kind: 'use', database: db || undefined, read: true, structure: false });
        continue;
      }
      if (/^db\s*\.\s*dropDatabase\s*\(/.test(part)) {
        info.statements.push({ kind: 'dropDatabase', database: db || undefined, read: false, structure: true });
        continue;
      }
      const m = mongoMethod(part);
      const out = MONGO_OUT.test(part);
      const read = !!m && MONGO_READ.test(m.method) && !out;
      const structure = out || MONGO_STRUCTURE.test(part);
      info.statements.push({ kind: m?.method ?? 'unknown', table: m?.coll, database: db || undefined, read, structure });
      continue;
    }
    const kind = firstWord(part).toUpperCase();
    const table = part.match(/(?:update|from|into|table)\s+([\w.]+)/i)?.[1];
    info.statements.push({ kind, table, read: statementKind(part) === 'read', structure: STRUCTURE.test(firstWord(part)) });
  }
  return info;
}
