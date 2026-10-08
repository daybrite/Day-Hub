// Shared app-owned UI strings, generated from the app's Fluent catalog. No English fallback.
import { readFileSync, mkdirSync, writeFileSync } from 'node:fs';
const source = readFileSync(new URL('../../resource/locales/en/app.ftl', import.meta.url), 'utf8');
const keys = ['callback_title', 'callback_help', 'callback_return', 'callback_home', 'callback_missing'];
const strings = Object.fromEntries(keys.map(key => {
  const match = source.match(new RegExp(`^${key} = (.+)$`, 'm'));
  if (!match) throw new Error(`Missing Fluent message: ${key}`);
  return [key, match[1]];
}));
const target = new URL('../src/generated/strings.mjs', import.meta.url);
mkdirSync(new URL('../src/generated/', import.meta.url), { recursive: true });
writeFileSync(target, '// Generated from resource/locales/en/app.ftl. Do not edit.\nexport const str = {\n' +
  Object.entries(strings).map(([key, value]) => `  ${key}: () => ${JSON.stringify(value)},`).join('\n') + '\n};\n');
