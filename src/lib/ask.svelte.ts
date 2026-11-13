import type { Component } from 'svelte';

/** What every asked dialog gets: `answer` closes it and resolves the caller's promise. */
export interface Asked<A> {
  answer: (result: A) => void;
}

interface Entry {
  id: number;
  dialog: Component<any>;
  props: Record<string, unknown>;
  resolve: (result: unknown) => void;
}

let seq = 0;

/** The open questions, innermost last; AskHost renders them in that order. */
export const asks = $state<{ list: Entry[] }>({ list: [] });

/** True while a question waits, so global shortcuts stay out of its way. */
export const asking = () => asks.list.length > 0;

/**
 * Opens `dialog` above everything and resolves with the value it answers; a dismissed dialog answers
 * `null`. The component gets the caller's props plus `answer`.
 */
export function ask<A>(dialog: Component<any>, props: Record<string, unknown> = {}): Promise<A> {
  return new Promise<A>((resolve) => {
    const id = ++seq;
    asks.list.push({
      id,
      dialog,
      props: { ...props, answer: (result: A) => answerAsk(id, result) },
      resolve: resolve as (result: unknown) => void,
    });
  });
}

export function answerAsk(id: number, result: unknown) {
  const at = asks.list.findIndex((e) => e.id === id);
  if (at < 0) return;
  const [entry] = asks.list.splice(at, 1);
  entry.resolve(result);
}
