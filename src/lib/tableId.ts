export interface TableId {
  connId: string;
  database: string;
  schema: string | null;
  name: string;
}

export const tableIdOf = (connId: string, t: { database: string; schema: string | null; name: string }): TableId => ({ connId, database: t.database, schema: t.schema, name: t.name });

/** The one key of a table: `conn/db/schema/name`, with an empty schema segment when it has none. */
export const tableKey = (id: TableId) => `${id.connId}/${id.database}/${id.schema ?? ''}/${id.name}`;

/**
 * Unquoted name for labels and autocomplete keys: PostgreSQL's `public` stays implicit, and MySQL and
 * SQLite have no schema. Snapshot tables, palette rows and the editor's table keys use this one rule.
 */
export const tableLabel = (t: { schema?: string | null; name: string }) => (t.schema && t.schema !== 'public' ? `${t.schema}.${t.name}` : t.name);
