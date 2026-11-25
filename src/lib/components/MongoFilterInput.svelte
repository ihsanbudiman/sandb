<script lang="ts">
  import { onMount } from 'svelte';
  import { EditorView, keymap, placeholder, drawSelection } from '@codemirror/view';
  import { EditorState, Prec } from '@codemirror/state';
  import { defaultKeymap, history, historyKeymap, indentMore, indentLess } from '@codemirror/commands';
  import { javascript } from '@codemirror/lang-javascript';
  import { syntaxHighlighting, HighlightStyle, indentUnit } from '@codemirror/language';
  import { acceptCompletion, autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap, startCompletion, type CompletionContext, type CompletionResult } from '@codemirror/autocomplete';
  import { tags as t } from '@lezer/highlight';
  import { MONGO_FILTER_OPERATORS, mongoElemMatchPath, mongoQueryPath } from '../mongo/filter';

  let {
    value = $bindable(''),
    fields,
    invalid = false,
    error = '',
    onapply,
  }: {
    value?: string;
    /** Schema field paths of the collection; array markers (`items[].sku`) complete as query paths. */
    fields: string[];
    invalid?: boolean;
    error?: string;
    onapply: () => void;
  } = $props();

  const operators = MONGO_FILTER_OPERATORS.map((label) => ({ label, type: 'keyword' }));
  const queryFields = $derived([...new Set(fields.map(mongoQueryPath).filter(Boolean))]);
  const plainKey = /^[A-Za-z_$][\w$]*$/;

  function keyPosition(state: EditorState, from: number): string | null {
    const before = state.sliceDoc(0, from);
    let i = before.length - 1;
    const quote = before[i] === '"' || before[i] === "'" ? before[i] : '';
    if (quote) i--;
    while (i >= 0 && /\s/.test(before[i])) i--;
    return before[i] === '{' || before[i] === ',' ? quote : null;
  }

  const escapeKey = (field: string, quote: string) => field.replace(/\\/g, '\\\\').replaceAll(quote, `\\${quote}`);

  function completions(ctx: CompletionContext): CompletionResult | null {
    const word = ctx.matchBefore(/[\w$.]*/);
    if (!word || (word.from === word.to && !ctx.explicit)) return null;
    const quote = keyPosition(ctx.state, word.from);
    if (quote === null) return null;
    if (word.text.startsWith('$') && !quote) return { from: word.from, options: operators, validFor: /^[\w$]*$/ };
    const elemPath = mongoElemMatchPath(ctx.state.doc.toString(), ctx.pos);
    const available = elemPath
      ? queryFields.filter((f) => f.startsWith(`${elemPath}.`)).map((f) => f.slice(elemPath.length + 1))
      : queryFields;
    if (!available.length) return null;
    const options = available.map((field) => {
      const insert = quote ? escapeKey(field, quote) : plainKey.test(field) ? field : JSON.stringify(field);
      return { label: field, type: 'property', apply: insert };
    });
    return { from: word.from, to: word.to, options, validFor: /^[\w$.]*$/ };
  }

  const hl = HighlightStyle.define([
    { tag: t.keyword, color: 'var(--c-accent)', fontWeight: '500' },
    { tag: [t.string, t.special(t.string)], color: 'var(--c-syn-string)' },
    { tag: t.number, color: 'var(--c-syn-number)' },
    { tag: t.comment, color: 'var(--c-faint)', fontStyle: 'italic' },
    { tag: [t.operator, t.punctuation], color: 'var(--c-muted)' },
    { tag: t.typeName, color: 'var(--c-syn-type)' },
  ]);

  const theme = EditorView.theme({
    '&': { minHeight: '28px', maxHeight: '112px', fontSize: 'var(--text-sm)', backgroundColor: 'transparent', color: 'var(--c-ink)' },
    '&.cm-focused': { outline: 'none' },
    '.cm-scroller': { fontFamily: "'JetBrains Mono Variable', ui-monospace, monospace", lineHeight: '1.5', overflow: 'auto' },
    '.cm-content': { padding: '3px 8px 4px', caretColor: 'var(--c-accent)' },
    '.cm-placeholder': { color: 'var(--c-faint)', fontFamily: 'var(--font-sans)' },
    '.cm-cursor': { borderLeftColor: 'var(--c-accent)', borderLeftWidth: '2px' },
    '.cm-selectionBackground, &.cm-focused .cm-selectionBackground': { backgroundColor: 'color-mix(in srgb, var(--c-accent) 24%, transparent) !important' },
    '.cm-tooltip': { zIndex: 'var(--z-index-menu)', backgroundColor: 'var(--c-surface)', border: '1px solid var(--c-line)', borderRadius: '8px', overflow: 'hidden', boxShadow: '0 12px 32px -8px rgba(0,0,0,.3)', fontFamily: "'JetBrains Mono Variable', ui-monospace, monospace", fontSize: 'var(--text-sm)', padding: '4px' },
    '.cm-tooltip-autocomplete ul li[aria-selected]': { backgroundColor: 'var(--c-active)', color: 'var(--c-ink)' },
    '.cm-tooltip.cm-tooltip-autocomplete > ul': { minWidth: '260px', maxWidth: '480px' },
    '.cm-tooltip-autocomplete ul li': { padding: '3px 8px !important', borderRadius: '5px' },
  });

  let el: HTMLDivElement;
  let view = $state.raw<EditorView>();

  onMount(() => {
    view = new EditorView({
      parent: el,
      state: EditorState.create({
        doc: value,
        extensions: [
          history(),
          drawSelection(),
          Prec.highest(keymap.of([
            {
              key: 'Enter',
              run: () => {
                onapply();
                return true;
              },
            },
            {
              key: 'Shift-Enter',
              run: (v) => {
                v.dispatch(v.state.replaceSelection('\n'), { scrollIntoView: true, userEvent: 'input' });
                return true;
              },
            },
          ])),
          keymap.of([
            {
              key: 'Tab',
              run: (v) => {
                if (acceptCompletion(v)) return true;
                if (v.state.selection.ranges.some((r) => !r.empty)) return indentMore(v);
                v.dispatch(v.state.replaceSelection(v.state.facet(indentUnit)), { scrollIntoView: true, userEvent: 'input' });
                return true;
              },
              shift: indentLess,
            },
            ...closeBracketsKeymap,
            ...completionKeymap,
            ...defaultKeymap,
            ...historyKeymap,
          ]),
          javascript(),
          autocompletion({ override: [completions] }),
          closeBrackets(),
          placeholder("Type a query: { field: 'value' }"),
          EditorView.contentAttributes.of({ 'aria-label': 'MongoDB filter' }),
          EditorView.domEventHandlers({
            focus: () => {
              if (!view || view.state.doc.length) return;
              // The click's DOM selection settles after focus; inserting in the same turn can leave
              // that selection spanning the inserted braces, so the first typed key would remove them.
              requestAnimationFrame(() => {
                if (!view || view.state.doc.length) return;
                view.dispatch({ changes: { from: 0, insert: '{}' }, selection: { anchor: 1 }, userEvent: 'input' });
                startCompletion(view);
              });
            },
          }),
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
    const v = value;
    if (view && v !== view.state.doc.toString()) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: v } });
    }
  });
</script>

<div bind:this={el} class="mongo-filter-input min-w-0 flex-1 rounded-md border bg-field transition-[border-color,box-shadow] focus-within:border-accent/70 focus-within:ring-3 focus-within:ring-accent/15 {invalid ? 'border-danger ring-2 ring-danger/25' : 'border-line'}" title={invalid ? error : undefined}></div>
