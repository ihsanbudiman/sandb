import type { Engine } from './api';
import { SKILLS } from './api/generated/constants';

/** A built-in instruction document of the assistant, as `load_skill` names it. */
export type Skill = (typeof SKILLS)[number];

const BY_NAME = new Map(SKILLS.map((s): [string, Skill] => [s.name, s]));

export const skillOf = (name: string): Skill | undefined => BY_NAME.get(name);

/** The skill's title; a name this build does not know stays as it is. */
export const skillTitle = (name: string): string => skillOf(name)?.title ?? name;

/** True when the skill applies to at least one of these engines. */
export function appliesTo(name: string, engines: readonly Engine[]): boolean {
  const skill = skillOf(name);
  return !!skill && engines.some((e) => skill.engines.includes(e));
}

/** The names that apply, each once, in the order the user picked them. */
export const applicable = (names: string[], engines: readonly Engine[]): string[] => [...new Set(names)].filter((n) => appliesTo(n, engines));
