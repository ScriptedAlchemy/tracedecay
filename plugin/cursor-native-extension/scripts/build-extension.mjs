import { constants } from 'node:fs';
import { access, mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

const extensionRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const embeddedPath = join(extensionRoot, 'embedded', 'extension.js');
const check = process.argv.slice(2).includes('--check');

const result = await build({
  absWorkingDir: extensionRoot,
  bundle: true,
  entryPoints: ['src/extension.ts'],
  external: ['vscode'],
  format: 'cjs',
  outfile: 'embedded/extension.js',
  platform: 'node',
  target: 'node20',
  write: false,
});
const compiled = result.outputFiles[0]?.contents;
if (compiled === undefined) {
  throw new Error('esbuild produced no Cursor extension JavaScript');
}

if (check) {
  await access(embeddedPath, constants.R_OK);
  const embedded = await readFile(embeddedPath);
  if (!embedded.equals(compiled)) {
    throw new Error(
      'embedded/extension.js is stale; run npm run build and commit the result',
    );
  }
} else {
  await mkdir(dirname(embeddedPath), { recursive: true });
  await writeFile(embeddedPath, compiled);
}
