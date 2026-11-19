import type { Transport } from '../backend';
import { ApiError } from '../types';
import type { CommandName } from '../generated/wire';
import { handlers, type Emit } from './handlers';
import { all } from './state';

const unknown = (name: CommandName) => new ApiError({ code: 'not_found', message: `The demo backend does not answer ${name}` });

/** The table's handlers type the arguments of their command; the transport hands them the parsed body. */
const handlersFor = handlers as unknown as Partial<Record<CommandName, (args: Record<string, unknown>, emit: Emit) => Promise<unknown>>>;

export const mockTransport: Transport = {
  async call<T>(name: CommandName, args: Record<string, unknown> | Uint8Array = {}): Promise<T> {
    const handler = handlersFor[name];
    const input = args instanceof Uint8Array ? {} : args;
    if (!handler) {
      const request = input.request;
      if (name.startsWith('export_') && request && typeof request === 'object' && 'connId' in request
        && all().some((conn) => conn.engine === 'redis' && conn.id === request.connId)) {
        throw new ApiError({ code: 'unsupported', message: 'Redis file export needs the desktop app' });
      }
      throw unknown(name);
    }
    return (await handler(input, () => {})) as T;
  },

  async stream(name, args, onEvent) {
    const handler = handlersFor[name];
    if (!handler) throw unknown(name);
    await handler(args, onEvent);
  },
};
