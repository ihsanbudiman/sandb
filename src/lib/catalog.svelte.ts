import { api, ApiError, type ColumnInfo, type SchemaSnapshot, type TableInfo, type TableProperties, type WriteInfo } from './api';
import { db, dbOf, type Conn, type Source } from './connections.svelte';
import { tableKey, type TableId } from './tableId';
import type { TableRef } from './tableActions';
import { classify, fromInfo, type Classification } from './writeCheck';

type Status = 'loading' | 'ok' | 'error';

export interface Loaded<T> {
  status: Status;
  items: T;
  error: string;
}

/**
 * Everything the app has loaded about schemas: databases, schemas, table lists, snapshots, columns,
 * table properties and DDL, each once per key and shared by the tree, the editor, the palette, the
 * properties view and the ER diagram. `rev` marks a structure change.
 */
export const catalog = $state({
  rev: 0,
  databases: {} as Record<string, Loaded<string[]>>,
  schemas: {} as Record<string, Loaded<string[]>>,
  tables: {} as Record<string, Loaded<TableInfo[]>>,
  snapshots: {} as Record<string, Loaded<SchemaSnapshot | null>>,
  columns: {} as Record<string, Loaded<ColumnInfo[]>>,
  properties: {} as Record<string, Loaded<TableProperties | null>>,
  ddl: {} as Record<string, Loaded<string>>,
});

/** The load sequence per key: a slow answer for an older request is dropped. */
const seq: Record<string, number> = {};
/** The table behind each table-keyed cache entry, for reloads and `forget`. */
const known: Record<string, TableId> = {};

const dbKey = (connId: string, database: string) => `${connId}/${database}`;
export const listKey = (connId: string, database: string, schema: string | null) => (schema === null ? dbKey(connId, database) : `${dbKey(connId, database)}/${schema}`);

/** The table list descriptor of each `<conn>/<db>[/<schema>]` key; reactive so the palette sees a new list. */
const lists = $state<Record<string, { connId: string; database: string; schema: string | null }>>({});

async function load<T>(box: Record<string, Loaded<T>>, store: string, key: string, empty: T, fn: () => Promise<T>): Promise<T> {
  const token = store + ':' + key;
  const n = (seq[token] = (seq[token] ?? 0) + 1);
  box[key] = { status: 'loading', items: box[key]?.items ?? empty, error: '' };
  try {
    const items = await fn();
    if (seq[token] === n) box[key] = { status: 'ok', items, error: '' };
    return items;
  } catch (e) {
    if (seq[token] === n) box[key] = { status: 'error', items: empty, error: ApiError.from(e).message };
    return empty;
  }
}

const ready = (l: Loaded<unknown> | undefined) => l !== undefined && l.status !== 'error';

export function databases(connId: string) {
  return catalog.databases[connId];
}

/** Loads the database list once; `force` reloads it (the database menus refetch on every open). */
export async function loadDatabases(connId: string, force = false) {
  if (!force && ready(catalog.databases[connId])) return catalog.databases[connId].items;
  return load(catalog.databases, 'dbs', connId, [], () => api.listDatabases({ connId }));
}

export function schemas(connId: string, database: string) {
  return catalog.schemas[dbKey(connId, database)];
}

export async function loadSchemas(connId: string, database: string, force = false) {
  const key = dbKey(connId, database);
  if (!force && ready(catalog.schemas[key])) return catalog.schemas[key].items;
  return load(catalog.schemas, 'schemas', key, [], () => api.listSchemas({ connId, database }));
}

export function tables(connId: string, database: string, schema: string | null) {
  return catalog.tables[listKey(connId, database, schema)];
}

export async function loadTables(connId: string, database: string, schema: string | null, force = false) {
  const key = listKey(connId, database, schema);
  lists[key] = { connId, database, schema };
  if (!force && ready(catalog.tables[key])) return catalog.tables[key].items;
  return load(catalog.tables, 'tables', key, [], () => api.listTables({ connId, database, schema }));
}

export function snapshot(connId: string, database: string) {
  return catalog.snapshots[dbKey(connId, database)];
}

export async function loadSnapshot(connId: string, database: string, force = false) {
  const key = dbKey(connId, database);
  if (!force && ready(catalog.snapshots[key])) return catalog.snapshots[key].items;
  return load<SchemaSnapshot | null>(catalog.snapshots, 'snap', key, null, () => api.schemaSnapshot({ connId, database }));
}

/** Remembered so a structure change can reload what a closed view loaded. */
function remember(id: TableId) {
  known[tableKey(id)] = id;
}

export function columns(connId: string, database: string, schema: string | null, table: string) {
  return catalog.columns[tableKey({ connId, database, schema, name: table })];
}

export async function loadColumns(connId: string, database: string, schema: string | null, table: string, force = false): Promise<ColumnInfo[]> {
  const id = { connId, database, schema, name: table };
  const key = tableKey(id);
  remember(id);
  if (!force && ready(catalog.columns[key])) return catalog.columns[key].items;
  return load(catalog.columns, 'cols', key, [], () => api.listColumns({ connId, database, schema, table }));
}

export function properties(connId: string, database: string, schema: string | null, table: string) {
  return catalog.properties[tableKey({ connId, database, schema, name: table })];
}

export async function loadProperties(connId: string, database: string, schema: string | null, table: string, force = false): Promise<TableProperties | null> {
  const id = { connId, database, schema, name: table };
  const key = tableKey(id);
  remember(id);
  if (!force && ready(catalog.properties[key])) return catalog.properties[key].items;
  return load<TableProperties | null>(catalog.properties, 'props', key, null, () => api.tableProperties({ connId, database, schema, table }));
}

export function ddl(connId: string, database: string, schema: string | null, table: string) {
  return catalog.ddl[tableKey({ connId, database, schema, name: table })];
}

export async function loadDdl(connId: string, database: string, schema: string | null, table: string, force = false): Promise<string> {
  const id = { connId, database, schema, name: table };
  const key = tableKey(id);
  remember(id);
  if (!force && ready(catalog.ddl[key])) return catalog.ddl[key].items;
  return load(catalog.ddl, 'ddl', key, '', () => api.tableDdl({ connId, database, schema, table }));
}

/** Every table the app has loaded, hidden partitions included, for the palette search. Partitions come last. */
export function loadedTables(): { c: Conn; t: TableRef; partition: boolean }[] {
  const out: { c: Conn; t: TableRef; partition: boolean }[] = [];
  for (const [key, l] of Object.entries(lists)) {
    const loaded = catalog.tables[key];
    if (!loaded || loaded.status !== 'ok') continue;
    const c = db.connections.find((x) => x.id === l.connId);
    if (!c) continue;
    for (const t of loaded.items) out.push({ c, t: { database: l.database, schema: l.schema, name: t.name, kind: t.kind }, partition: !!t.partitionOf });
  }
  return out.sort((a, b) => +a.partition - +b.partition);
}

/** Whether the loaded list already names this collection. A Mongo write into an unlisted name may create it. */
export function hasCollection(c: Conn, database: string, collection: string): boolean {
  const loaded = catalog.tables[listKey(c.id, database, null)];
  return loaded?.status === 'ok' && loaded.items.some((t) => t.name === collection);
}

/**
 * Autocomplete columns of a table the snapshot holds without columns (cut by the column cap), or of a
 * PG table outside `public` written without its schema (only when the name is unique).
 */
export async function completionColumns(connId: string, database: string, path: string[]): Promise<string[]> {
  if (path.length > 2) return [];
  const snap = await loadSnapshot(connId, database);
  if (!snap) return [];
  const [ns, name] = path.length === 2 ? path : [undefined, path[0]];
  const hits = snap.tables.filter((t) => t.name === name && (ns === undefined || t.schema === ns));
  const t = hits.find((x) => !x.schema || x.schema === 'public') ?? (hits.length === 1 ? hits[0] : undefined);
  if (!t) return [];
  if (t.columns.length) return t.columns.map((x) => x.name);
  return (await loadColumns(connId, database, t.schema ?? null, t.name)).map((x) => x.name);
}

export function reloadLists(connId: string) {
  for (const key of Object.keys(catalog.databases)) if (key === connId) void loadDatabases(connId, true);
  for (const key of Object.keys(catalog.schemas)) if (key.startsWith(connId + '/')) void loadSchemas(connId, key.slice(connId.length + 1), true);
  for (const [key, l] of Object.entries(lists)) if (key.startsWith(connId + '/')) void loadTables(l.connId, l.database, l.schema, true);
  for (const key of Object.keys(catalog.snapshots)) if (key.startsWith(connId + '/')) void loadSnapshot(connId, key.slice(connId.length + 1), true);
}

/**
 * A structure change on `c`: the catalog revision moves, loaded table lists and snapshots reload from
 * the server, and the columns, properties and DDL of the connection load again. Callers do this one
 * thing; open views follow `rev`.
 */
export function structureChanged(c: Conn) {
  const id = c.id;
  catalog.rev++;
  for (const key of Object.keys(known)) {
    if (!key.startsWith(id + '/')) continue;
    const t = known[key];
    if (catalog.columns[key]) void loadColumns(t.connId, t.database, t.schema, t.name, true);
    if (catalog.properties[key]) void loadProperties(t.connId, t.database, t.schema, t.name, true);
    if (catalog.ddl[key]) void loadDdl(t.connId, t.database, t.schema, t.name, true);
  }
  reloadLists(id);
}

/** A connection now points somewhere else (or is gone): everything loaded for it is stale. */
export function forget(c: Conn) {
  const id = c.id;
  // The revision moves so views that watch it reload against the new target.
  catalog.rev++;
  const mine = (k: string) => k === id || k.startsWith(id + '/');
  for (const box of [catalog.databases, catalog.schemas, catalog.tables, catalog.snapshots, catalog.columns, catalog.properties, catalog.ddl]) {
    for (const k of Object.keys(box)) if (mine(k)) delete box[k];
  }
  for (const k of Object.keys(lists)) if (k === id || k.startsWith(id + '/')) delete lists[k];
  for (const k of Object.keys(known)) if (mine(k)) delete known[k];
  // A request already in flight must not write its answer into the cleared entries.
  for (const k of Object.keys(seq)) seq[k]++;
}

export function afterRun(c: Conn, cls: Classification, fallback: string) {
  // A structure change, or text no classifier could read (it may hide any write): reload.
  if (cls.structure || !cls.info) return structureChanged(c);
  if (c.engine !== 'mongo') return;
  // A Mongo write into a collection the tree does not list may have created it. A write with no
  // collection (`dropDatabase`) changed the database list, which its `structure` flag already covers.
  const writes = cls.info.statements.filter((s) => s.read === false);
  if (writes.some((s) => !s.table || !hasCollection(c, s.database ?? fallback, s.table))) structureChanged(c);
}

/**
 * The catalog check of one written statement; `source` is the database it ran on, else the
 * connection's. `info` is the classification the user approved, so it is not asked again.
 */
export async function afterWrite(c: Conn, sql: string, source?: Source, info?: WriteInfo) {
  const database = source ? dbOf(source, c) : c.database;
  const cls = info ? fromInfo(c.engine, sql, info) : await classify(c.engine, sql, database);
  afterRun(c, cls, database);
}
