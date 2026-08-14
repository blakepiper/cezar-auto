// Node 25+ exposes a guarded global `localStorage` accessor that is undefined unless
// `--localstorage-file` is supplied. The cockpit tests use jsdom's per-window storage instead;
// mirror it onto the global so browser code and existing tests see the same API on every
// supported Node version.
const memoryStorage = () => {
  const values = new Map<string, string>();
  return {
    get length() { return values.size; },
    clear: () => values.clear(),
    getItem: (key: string) => values.get(key) ?? null,
    key: (index: number) => [...values.keys()][index] ?? null,
    removeItem: (key: string) => { values.delete(key); },
    setItem: (key: string, value: string) => { values.set(String(key), String(value)); },
  };
};

const storage = typeof window !== 'undefined' && window.localStorage !== undefined
  ? window.localStorage
  : memoryStorage();
Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: storage });
