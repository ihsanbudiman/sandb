<script lang="ts">
  import { onMount } from 'svelte';
  import { EditorView, keymap, lineNumbers, highlightActiveLine, drawSelection } from '@codemirror/view';
  import { EditorState, Compartment, type Text } from '@codemirror/state';
  import { defaultKeymap, history, historyKeymap, selectAll as selectAllCmd } from '@codemirror/commands';
  import { sql, PostgreSQL, MySQL, SQLite } from '@codemirror/lang-sql';
  import { javascript } from '@codemirror/lang-javascript';
  import { syntaxHighlighting, HighlightStyle, syntaxTree, foldService, foldGutter, foldedRanges, foldEffect, unfoldEffect, foldAll, unfoldAll, codeFolding } from '@codemirror/language';
  import { autocompletion, type CompletionContext, type CompletionResult } from '@codemirror/autocomplete';
  import { tags as t } from '@lezer/highlight';
  import { columnCompletion } from '../sql/columns';
  import type { Engine } from '../api';
  import { statementRanges, maskLiterals } from '../sql/text';
  import { mongoStatementRanges } from '../mongo/text';
  import { MONGOSH_COLLECTION_METHODS, MONGOSH_CURSOR_METHODS, MONGOSH_DB_METHODS } from '../api/generated/constants';
  import { formatSql } from '../sql/format';
  import { formatMongo } from '../mongo/format';
  import { KEYS } from '../keys';
  import { notify } from '../notice.svelte';

  let {
    value = $bindable(''),
    onrun,
    onrunnew,
    onrunall,
    onformat,
    schema = {},
    dialect = 'pg',
    columnsOf,
  }: {
    value?: string;
    /** Runs the selection, or the statement at the cursor. */
    onrun?: () => void;
    /** Like `onrun`, but the results open in new result tabs. */
    onrunnew?: () => void;
    /** Runs every statement in the editor (Alt+X, as in DBeaver). */
    onrunall?: () => void;
    /** Formats the selection, or all the text (⇧⌘F). */
    onformat?: () => void;
    schema?: Record<string, string[]>;
    dialect?: Engine;
    /** Columns of a table that `schema` holds without columns (or not under this name); [] when unknown. */
    columnsOf?: (path: string[]) => Promise<string[]>;
  } = $props();

  /** The SQL dialect of the text; a mongosh tab gets JavaScript and the mongosh helpers instead. */
  const sqlDialect = $derived(dialect === 'mongo' ? null : dialect);

  const lang = new Compartment();
  // A mongo tab speaks mongosh (JavaScript with `db.` calls), not SQL.
  const langExt = () =>
    sqlDialect ? sql({ dialect: { pg: PostgreSQL, mysql: MySQL, sqlite: SQLite }[sqlDialect], schema, upperCaseKeywords: true }) : javascript();

  // Fetched columns per `schema` object; a new snapshot clears them.
  let fetched = new Map<string, Promise<string[]>>();
  let fetchedFor: Record<string, string[]> | undefined;
  /** The last completion failure that was shown, so a repeated one does not stack notices. */
  let reported = '';

  /** Query, update and aggregation operators. */
  const mongoOps = ['$eq', '$ne', '$gt', '$gte', '$lt', '$lte', '$in', '$nin', '$and', '$or', '$nor', '$not', '$exists', '$type', '$regex', '$elemMatch', '$size', '$all', '$set', '$unset', '$inc', '$push', '$pull', '$addToSet', '$match', '$group', '$project', '$sort', '$limit', '$skip', '$lookup', '$unwind', '$addFields', '$count', '$facet', '$sum', '$avg', '$min', '$max', '$first', '$last'];

  /** `FROM t a` / `FROM t AS a` aliases of the statement at `pos` → table path. Same scan as lang-sql. */
  function aliasesAt(state: EditorState, pos: number): Record<string, string[]> {
    const out: Record<string, string[]> = {};
    let stmt = syntaxTree(state).resolveInner(pos, -1);
    while (stmt.name !== 'Statement') {
      if (!stmt.parent) return out;
      stmt = stmt.parent;
    }
    const isId = (n: typeof stmt | null) => n?.name === 'Identifier' || n?.name === 'QuotedIdentifier';
    const text = (n: typeof stmt) => state.sliceDoc(n.from, n.to).replace(/^(["`])(.*)\1$/, '$2');
    const pathOf = (n: typeof stmt) => (n.name === 'CompositeIdentifier' ? n.getChildren('Identifier').concat(n.getChildren('QuotedIdentifier')).sort((a, b) => a.from - b.from).map(text) : [text(n)]);
    let sawFrom = false;
    let prev: typeof stmt | null = null;
    for (let n = stmt.firstChild; n; n = n.nextSibling) {
      const kw = n.name === 'Keyword' ? state.sliceDoc(n.from, n.to).toLowerCase() : null;
      let alias: string | null = null;
      if (!sawFrom) sawFrom = kw === 'from';
      else if (kw === 'as' && prev && isId(n.nextSibling)) alias = text(n.nextSibling!);
      else if (kw && /^(where|group|having|order|union|intersect|except|all|distinct|limit|offset|fetch|for)$/.test(kw)) break;
      else if (prev && isId(n)) alias = text(n);
      if (alias && prev) out[alias] = pathOf(prev);
      prev = /Identifier$/.test(n.name) ? n : null;
    }
    return out;
  }

  /**
   * Columns of one table: from the snapshot when it has them, else from the database (the snapshot's
   * column cap cut them, or a PG table outside `public` is written without its schema). Cached per
   * snapshot, so typing does not ask again.
   */
  async function loadColumns(path: string[]): Promise<string[]> {
    const key = path.join('.');
    if (schema[key]?.length) return schema[key];
    // The snapshot names a `public` PG table (and a `main` SQLite table) without its schema.
    const known = sqlDialect === 'pg' && path.length === 2 && path[0] === 'public' ? schema[path[1]] : sqlDialect === 'sqlite' && path.length === 2 && path[0] === 'main' ? schema[path[1]] : undefined;
    if (known?.length) return known;
    if (!columnsOf) return [];
    if (fetchedFor !== schema) {
      fetched = new Map();
      fetchedFor = schema;
    }
    let p = fetched.get(key);
    if (!p) {
      const cache = fetched;
      p = columnsOf(path).catch((e) => {
        // A failed fetch must not empty completion silently: retry next time and say it once.
        cache.delete(key);
        const text = e instanceof Error ? e.message : String(e);
        if (text !== reported) notify(false, `Column completion failed: ${text}`);
        reported = text;
        return [];
      });
      cache.set(key, p);
    }
    return p;
  }

  /**
   * Columns after `table.` or `alias.` when the snapshot has none for that table: it cut them (column
   * cap) or names the table differently (a PG table outside `public`, written without its schema).
   */
  async function lazyColumns(ctx: CompletionContext): Promise<CompletionResult | null> {
    const m = /((?:(?:[A-Za-z_][\w$]*|"[^"]+"|`[^`]+`)\.)+)([\w$]*)$/.exec(ctx.state.sliceDoc(ctx.state.doc.lineAt(ctx.pos).from, ctx.pos));
    if (!m || !columnsOf) return null;
    let path = m[1].slice(0, -1).match(/"[^"]+"|`[^`]+`|[^.]+/g)!.map((p) => p.replace(/^(["`])(.*)\1$/, '$2'));
    if (path.length === 1) path = aliasesAt(ctx.state, ctx.pos)[path[0]] ?? path;
    if (schema[path.join('.')]?.length) return null; // lang-sql completes it
    const cols = await loadColumns(path);
    if (!cols.length || ctx.aborted) return null;
    return { from: ctx.pos - m[2].length, options: cols.map((label) => ({ label, type: 'property' })), validFor: /^[\w$]*$/ };
  }

  /** Columns of the tables in the statement at the cursor, so a plain `SELECT col…` lists them. */
  const columnsInScope = (ctx: CompletionContext) => (sqlDialect ? columnCompletion(ctx, { dialect: sqlDialect, schema, load: loadColumns }) : null);

  /** Statement ranges of the editor text: mongosh splitting for mongo, SQL splitting otherwise. */
  const rangesOf = (doc: string, blankLines = false): { from: number; to: number }[] =>
    sqlDialect ? statementRanges(doc, sqlDialect, blankLines).map(([from, to]) => ({ from, to })) : mongoStatementRanges(doc);

  /** The collection a `db.<coll>.` / `db.getCollection("<coll>").` call before `pos` targets, or ''. */
  function mongoCollectionBefore(state: EditorState, pos: number): string {
    const before = state.sliceDoc(0, pos);
    const m = /\bdb\.(?:getCollection\(\s*(['"])([^'"]+)\1\s*\)|([\w$]+))\.[\w$]*\([^()]*$/.exec(before);
    return m ? m[2] ?? m[3] : '';
  }

  /** Mongosh completions: collections after `db.`, methods after `db.<coll>.`, `$` operators in args. */
  function mongoCompletions(ctx: CompletionContext): CompletionResult | null {
    const line = ctx.state.sliceDoc(ctx.state.doc.lineAt(ctx.pos).from, ctx.pos);
    const word = /\w*$/.exec(line)?.[0] ?? '';
    // `$` operators inside filter/update documents.
    if (word.startsWith('$')) return { from: ctx.pos - word.length, options: mongoOps.map((label) => ({ label, type: 'keyword' })), validFor: /^[\w$]*$/ };
    // Methods after `db.<coll>.` or `db.getCollection("<coll>").`.
    if (/\bdb\.(?:[\w$]+|getCollection\(\s*(['"])[^'"]+\1\s*\))\.[\w$]*$/.test(line))
      return { from: ctx.pos - word.length, options: MONGOSH_COLLECTION_METHODS.map((label) => ({ label, type: 'method' })), validFor: /^[\w$]*$/ };
    // Cursor methods after a `find(...)` call, possibly chained.
    if (/\bfind\([^()]*\)\s*(?:\.\w+\(\)\s*)*\.$/.test(line))
      return { from: ctx.pos - word.length, options: MONGOSH_CURSOR_METHODS.map((label) => ({ label, type: 'method' })), validFor: /^[\w$]*$/ };
    // Collections after `db.`, plus the shell helpers that take one further step.
    if (/\bdb\.[\w$]*$/.test(line)) {
      const cols = Object.keys(schema).map((k) => k.split('.').at(-1)!);
      const opts = [...new Set([...cols, ...MONGOSH_DB_METHODS])].map((label) => ({ label, type: 'property' }));
      return { from: ctx.pos - word.length, options: opts, validFor: /^[\w$]*$/ };
    }
    // Field paths inside the argument braces of a `db.<coll>.<method>(` call.
    const coll = mongoCollectionBefore(ctx.state, ctx.pos);
    if (coll) {
      const fields = schema[coll] ?? [];
      const opts = fields.map((f) => (f.includes('.') ? { label: f, apply: `"${f}"`, type: 'property' } : { label: f, type: 'property' }));
      if (opts.length) return { from: ctx.pos - word.length, options: opts, validFor: /^[\w$]*$/ };
    }
    return null;
  }

  /** The editor commands App uses; one handle instead of seven component exports. */
  export interface EditorCommands {
    focus(): void;
    /** The selected text, or '' when nothing is selected. */
    selection(): string;
    /** Replaces the selection (or inserts at the cursor), as a paste would. */
    insert(text: string): void;
    selectAll(): void;
    /** Formats the selection, or all the text when nothing is selected, as one undo step. */
    format(): void;
    /** A right-click outside the selection puts the cursor there, so "statement at cursor" is the clicked one. */
    pointAt(x: number, y: number): void;
    /**
     * The statement under the cursor, `;` included; a blank line also ends a statement, as in DBeaver.
     * On a blank line it takes the next statement; after the last one, the last one. '' when the editor
     * holds no statement.
     */
    statementAt(): string;
  }

  /** The selection, or the statement at the cursor when nothing is selected. */
  function selection() {
    const r = view?.state.selection.main;
    return view && r && !r.empty ? view.state.sliceDoc(r.from, r.to) : '';
  }

  function insert(text: string) {
    if (!view) return;
    view.dispatch(view.state.replaceSelection(text));
    view.focus();
  }

  function selectAll() {
    if (view) selectAllCmd(view);
    view?.focus();
  }

  /**
   * Formats the selection, or all the text when nothing is selected, as one undo step. The
   * formatted selection stays selected. Throws when the text cannot be read (unbalanced brackets,
   * an unclosed string, SQL that the formatter does not know).
   */
  function format() {
    if (!view) return;
    const r = view.state.selection.main;
    const [from, to] = r.empty ? [0, view.state.doc.length] : [r.from, r.to];
    const text = view.state.sliceDoc(from, to);
    const out = sqlDialect ? formatSql(text, sqlDialect) : formatMongo(text);
    if (out !== text) view.dispatch({ changes: { from, to, insert: out }, selection: r.empty ? undefined : { anchor: from, head: from + out.length }, scrollIntoView: true });
    view.focus();
  }

  function pointAt(x: number, y: number) {
    const pos = view?.posAtCoords({ x, y });
    const r = view?.state.selection.main;
    if (view && r && pos != null && (r.empty || pos < r.from || pos > r.to)) view.dispatch({ selection: { anchor: pos } });
  }

  function statementAt() {
    if (!view) return '';
    const doc = view.state.doc.toString();
    const head = view.state.selection.main.head;
    const parts = rangesOf(doc, true)
      .map(({ from, to }) => ({ to, text: doc.slice(from, to).trim() }))
      .filter((p) => p.text && p.text !== ';');
    return (parts.find((p) => head <= p.to) ?? parts.at(-1))?.text ?? '';
  }

  export function commands(): EditorCommands {
    return { focus: () => view?.focus(), selection, insert, selectAll, format, pointAt, statementAt };
  }

  /** Fold range of each statement that spans lines: from the end of its first line to its end. */
  type Fold = { from: number; to: number; end: number };
  let foldDoc: Text | null = null;
  let foldDialect: Engine | null = null;
  let folds: Fold[] = [];

  function statementFolds(state: EditorState): Fold[] {
    if (state.doc === foldDoc && dialect === foldDialect) return folds;
    const doc = state.doc.toString();
    const out: Fold[] = [];
    for (const { from, to } of rangesOf(doc, true)) {
      const raw = doc.slice(from, to);
      // Comments and strings are blanked; a comments-only range gets nothing. Mongosh text has no SQL
      // dialect, so it takes the plain rules (no `#` comments, no backslash escapes, no dollar quotes).
      const masked = maskLiterals(raw, sqlDialect ?? 'sqlite');
      if (!masked.trim()) continue;
      const first = state.doc.lineAt(from + masked.length - masked.trimStart().length);
      const last = from + masked.trimEnd().length;
      // `end` is the range end `statementAt` uses, so the cursor picks the same statement.
      if (last > first.to && !(out.length && first.from <= out[out.length - 1].to)) out.push({ from: first.to, to: last, end: to });
    }
    foldDoc = state.doc;
    foldDialect = dialect;
    folds = out;
    return out;
  }

  /** Each statement folds from the line it starts on, like a block in VS Code. */
  const statementFold = foldService.of((state, lineStart, lineEnd) => statementFolds(state).find((f) => f.from === lineEnd && f.from >= lineStart) ?? null);

  /** Folds the statement at the cursor (any line of it), or unfolds it when it is folded. */
  function toggleStatementFold(v: EditorView) {
    const head = v.state.selection.main.head;
    const list = statementFolds(v.state);
    const f = list.find((f) => head <= f.end && head >= v.state.doc.lineAt(f.from).from);
    if (!f) return false;
    let folded = false;
    foldedRanges(v.state).between(f.from, f.from, (from) => {
      if (from === f.from) folded = true;
    });
    v.dispatch({ effects: (folded ? unfoldEffect : foldEffect).of({ from: f.from, to: f.to }) });
    return true;
  }

  const chevron = (open: boolean) => {
    const s = document.createElement('span');
    s.className = 'cm-fold-chevron';
    s.title = open ? 'Fold statement' : 'Unfold statement';
    s.innerHTML = `<svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true"><path d="${open ? 'M3 4.5 6 7.5 9 4.5' : 'M4.5 3 7.5 6 4.5 9'}" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
    return s;
  };

  let el: HTMLDivElement;
  let view = $state.raw<EditorView>();

  const hl = HighlightStyle.define([
    { tag: t.keyword, color: 'var(--c-accent)', fontWeight: '500' },
    { tag: [t.string, t.special(t.string)], color: 'var(--c-syn-string)' },
    { tag: t.number, color: 'var(--c-syn-number)' },
    { tag: t.comment, color: 'var(--c-faint)', fontStyle: 'italic' },
    { tag: [t.operator, t.punctuation], color: 'var(--c-muted)' },
    { tag: t.typeName, color: 'var(--c-syn-type)' },
  ]);

  const theme = EditorView.theme({
    '&': { height: '100%', fontSize: '13px', backgroundColor: 'transparent', color: 'var(--c-ink)' },
    '&.cm-focused': { outline: 'none' },
    '.cm-scroller': { fontFamily: "'JetBrains Mono Variable', ui-monospace, monospace", lineHeight: '1.6' },
    '.cm-content': { padding: '2px 0 8px' },
    // Opaque: the gutter is sticky, and code scrolled sideways under a transparent gutter shows through the line numbers.
    '.cm-gutters': { backgroundColor: 'var(--c-surface)', border: 'none', color: 'var(--c-faint)', paddingLeft: '10px', paddingRight: '4px' },
    '.cm-foldGutter .cm-gutterElement': { display: 'flex', alignItems: 'center', justifyContent: 'center', padding: '0 2px' },
    '.cm-fold-chevron': { display: 'grid', placeItems: 'center', width: '14px', height: '18px', color: 'var(--c-faint)', cursor: 'pointer' },
    '.cm-fold-chevron:hover': { color: 'var(--c-ink)' },
    '.cm-foldPlaceholder': { backgroundColor: 'var(--c-hover)', border: 'none', borderRadius: '4px', color: 'var(--c-muted)', padding: '0 5px', margin: '0 4px' },
    '.cm-activeLine': { backgroundColor: 'var(--c-hover)' },
    '.cm-activeLineGutter': { backgroundColor: 'transparent', color: 'var(--c-ink)' },
    '.cm-cursor': { borderLeftColor: 'var(--c-accent)', borderLeftWidth: '2px' },
    '.cm-selectionBackground, &.cm-focused .cm-selectionBackground': { backgroundColor: 'color-mix(in srgb, var(--c-accent) 24%, transparent) !important' },
    '.cm-tooltip': { backgroundColor: 'var(--c-surface)', border: '1px solid var(--c-line)', borderRadius: '8px', overflow: 'hidden', boxShadow: '0 12px 32px -8px rgba(0,0,0,.3)', fontFamily: "'JetBrains Mono Variable', ui-monospace, monospace", fontSize: '12px', padding: '4px' },
    '.cm-tooltip-autocomplete ul li': { padding: '3px 8px !important', borderRadius: '5px' },
    '.cm-tooltip-autocomplete ul li[aria-selected]': { backgroundColor: 'var(--c-active)', color: 'var(--c-ink)' },
  });

  onMount(() => {
    view = new EditorView({
      parent: el,
      state: EditorState.create({
        doc: value,
        extensions: [
          codeFolding({ placeholderText: '…' }),
          statementFold,
          foldGutter({ markerDOM: chevron }),
          lineNumbers(),
          // CodeMirror draws the cursor: WebKit (Tauri) drops the native caret under the glass panels.
          drawSelection(),
          highlightActiveLine(),
          history(),
          keymap.of([
            { key: KEYS.run.combo, run: () => (onrun?.(), true) },
            { key: KEYS.runNew.combo, run: () => (onrunnew?.(), true) },
            { key: KEYS.runAll.combo, run: () => (onrunall?.(), true) },
            { key: KEYS.format.combo, run: () => (onformat?.(), true) },
            { key: KEYS.foldStatement.combo, run: toggleStatementFold },
            { key: KEYS.foldAll.combo, run: foldAll },
            { key: KEYS.unfoldAll.combo, run: unfoldAll },
            ...defaultKeymap,
            ...historyKeymap,
          ]),
          lang.of(langExt()),
          autocompletion(),
          EditorState.languageData.of(() =>
            sqlDialect ? [{ autocomplete: columnsInScope }, { autocomplete: lazyColumns }] : [{ autocomplete: mongoCompletions }],
          ),
          syntaxHighlighting(hl),
          theme,
          EditorView.updateListener.of((u) => {
            if (u.docChanged) value = u.state.doc.toString();
          }),
        ],
      }),
    });
    return () => view?.destroy();
  });

  $effect(() => {
    const ext = langExt();
    view?.dispatch({ effects: lang.reconfigure(ext) });
  });

  // Sync when the parent replaces the text (e.g. clicking a table in the sidebar).
  $effect(() => {
    const v = value;
    if (view && v !== view.state.doc.toString()) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: v } });
    }
  });
</script>

<div bind:this={el} class="h-full overflow-hidden"></div>
