import { StreamLanguage } from '@codemirror/language';
import { redisQuoteEnd } from './text';

export const redisLanguage = StreamLanguage.define({
  startState: () => ({ first: true, quote: '' }),
  token(stream, state) {
    if (stream.sol()) state.first = !state.quote;
    if (!state.quote) {
      if (stream.eatWhile(/[ \t\r]/)) return null;
      if (state.first && stream.peek() === '#') {
        stream.skipToEnd();
        return 'comment';
      }
      const c = stream.peek();
      if (c === '"' || c === "'") {
        state.quote = c;
        stream.next();
      }
    }
    const first = state.first;
    state.first = false;
    if (state.quote) {
      const end = redisQuoteEnd(stream.string, stream.pos, state.quote);
      if (end < 0) stream.skipToEnd();
      else {
        stream.pos = end;
        state.quote = '';
        // Keep an invalid suffix in this token, so a quote in it does not start another argument.
        stream.eatWhile(/[^ \t\r]/);
      }
      return 'string';
    }
    stream.eatWhile(/[^ \t\r]/);
    if (first) return 'keyword';
    return /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/.test(stream.current()) ? 'number' : null;
  },
});
