import { api, type Engine, type WriteInfo } from './api';
import { mongoStatementRanges } from './mongo/text';
import { statementKind, statementRanges } from './sql/text';

/** What the text holds, without asking the backend. */
export interface Statements {
  count: number;
  /** Nothing to run: blank text, comments only, or no statement the parser finds. */
  empty: boolean;
  write: boolean;
}

/** The local split of `text`: the same statement rules the editor and the Run menu use. */
export function statements(engine: Engine, text: string): Statements {
  if (engine === 'mongo') {
    const count = mongoStatementRanges(text).length;
    return { count, empty: count === 0, write: false };
  }
  const parts = statementRanges(text, engine).filter(([from, to]) => statementKind(text.slice(from, to)) !== 'empty');
  return {
    count: parts.length,
    empty: parts.length === 0,
    write: parts.some(([from, to]) => statementKind(text.slice(from, to)) === 'write'),
  };
}

export interface Classification extends Statements {
  /** At least one statement changes the schema, the collection list or the rows a `find` would see. */
  structure: boolean;
  /** The backend's answer; null when it could not classify, which counts as unparsable. */
  info: WriteInfo | null;
  /** The text cannot be parsed: it is treated as a write, so nothing runs from a guess. */
  unparsable: boolean;
}

/** The classification of one backend answer: the local split plus the guard's per-statement facts. */
export function fromInfo(engine: Engine, text: string, info: WriteInfo): Classification {
  const local = statements(engine, text);
  const kinds = info.statements;
  const unparsable = info.warnings.some((w) => w.code === 'unparsable');
  // SQL statements send no `read`, so only a Mongo statement can say it only reads.
  const writes = kinds.filter((s) => s.read !== true);
  // Both classifiers send `structure`; a statement without the flag changes nothing.
  const structure = kinds.some((s) => s.structure === true);
  if (engine === 'mongo') {
    // Comment-only text splits into a range but classifies as no statements: nothing to run.
    const empty = local.count === 0 || (!kinds.length && !unparsable);
    return { count: local.count, empty, write: !empty && (unparsable || writes.length > 0), structure, unparsable, info };
  }
  return { ...local, structure, unparsable, info };
}

/**
 * The one classification of a run: the empty check, read vs write, a schema/collection change and the
 * backend's warnings. A failed classification is never guessed around: the text counts as an unparsable
 * write, so a production Mongo run still asks before it runs.
 */
export async function classify(engine: Engine, text: string, database?: string | null): Promise<Classification> {
  try {
    return fromInfo(engine, text, await api.classifyWrite({ engine, sql: text, database: database ?? null }));
  } catch {
    // No preview is available; `unparsable` covers it.
    const local = statements(engine, text);
    return { ...local, write: engine === 'mongo' ? !local.empty : local.write, structure: false, unparsable: true, info: null };
  }
}
