import { invoke, Channel } from '@tauri-apps/api/core';
import type { Transport } from './backend';
import { ApiError } from './types';
import type { ChatEvent, CommandName } from './generated/wire';

// The channel orders its messages by index and delivers them in send order; `chat_send` sends the terminal
// `done`/`error` before it returns, so the stream resolves when that event arrives. An invoke failure rejects.
export const tauriTransport: Transport = {
  async call<T>(name: CommandName, args: Record<string, unknown> | Uint8Array = {}, headers?: Record<string, string>): Promise<T> {
    try {
      return await invoke<T>(name, args, headers && { headers });
    } catch (e) {
      throw ApiError.from(e);
    }
  },

  async stream(name, args, onEvent) {
    let settle: () => void = () => {};
    const terminal = new Promise<void>((resolve) => (settle = resolve));
    const channel = new Channel<ChatEvent>();
    channel.onmessage = (e) => {
      onEvent(e);
      if (e.type === 'done' || e.type === 'error') settle();
    };
    await tauriTransport.call<void>(name, { ...args, onEvent: channel });
    await terminal;
  },
};
