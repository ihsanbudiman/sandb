import type { Transport } from '../backend';
import { ApiError } from '../types';
import type { CommandName } from '../generated/wire';
import { handlers, type Emit } from './handlers';

const unknown = (name: CommandName) => new ApiError({ code: 'not_found', message: `The demo backend does not answer ${name}` });

/** The table's handlers type the arguments of their command; the transport hands them the parsed body. */
const handlersFor = handlers as unknown as Partial<Record<CommandName, (args: Record<string, unknown>, emit: Emit) => Promise<unknown>>>;

export const mockTransport: Transport = {
  async call<T>(name: CommandName, args: Record<string, unknown> | Uint8Array = {}): Promise<T> {
    const handler = handlersFor[name];
    if (!handler) throw unknown(name);
    const input = args instanceof Uint8Array ? {} : args;
    return (await handler(input, () => {})) as T;
  },

  async stream(name, args, onEvent) {
    const handler = handlersFor[name];
    if (!handler) throw unknown(name);
    await handler(args, onEvent);
  },
};
