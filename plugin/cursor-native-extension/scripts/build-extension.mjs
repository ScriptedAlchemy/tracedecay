import { constants } from 'node:fs';
import { access, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRslib } from '@rslib/core';

const extensionRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const embeddedPath = join(extensionRoot, 'embedded', 'extension.js');
const check = process.argv.slice(2).includes('--check');

// Build into a throwaway directory so --check never touches the committed
// bytes, then byte-compare/copy. Output must stay deterministic: no minify,
// no source maps, no content hashes, only `vscode` external (the language
// client is bundled so the embedded asset is self-contained).
const outDir = await mkdtemp(join(tmpdir(), 'tracedecay-cursor-ext-'));
let compiled;
try {
  const rslib = await createRslib({
    cwd: extensionRoot,
    config: {
      root: extensionRoot,
      lib: [
        {
          format: 'cjs',
          syntax: ['node 20'],
          bundle: true,
          autoExternal: false,
          source: {
            entry: { extension: join(extensionRoot, 'src', 'extension.ts') },
          },
          output: {
            target: 'node',
            distPath: { root: outDir },
            filename: { js: '[name].js' },
            externals: { vscode: 'commonjs vscode' },
            minify: false,
            sourceMap: false,
          },
        },
      ],
    },
  });
  await rslib.build();
  compiled = await readFile(join(outDir, 'extension.js'));
} finally {
  await rm(outDir, { recursive: true, force: true });
}
if (compiled.length === 0) {
  throw new Error('rslib produced no Cursor extension JavaScript');
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
