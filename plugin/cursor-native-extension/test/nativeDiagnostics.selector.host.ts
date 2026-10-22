import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import * as vscode from "vscode";
import { LanguageClient } from "vscode-languageclient/node";

import { nativeDiagnosticsDocumentSelector } from "../src/nativeDiagnostics.js";

export async function runNativeDiagnosticsSelectorHostTest() {
  const temporaryRoot = await mkdtemp(
    join(tmpdir(), "tracedecay-cursor-selector-"),
  );
  const rootNames = [
    "ordinary",
    "project with spaces",
    "工程 café",
    "project[1]",
    "project{a,b}",
    ...(process.platform === "win32" ? [] : ["project*", "project?"]),
  ];
  const results: Array<{
    root: string;
    rootDocument: boolean;
    nestedDocument: boolean;
    siblingDocument: boolean;
    untitledDocument: boolean;
  }> = [];
  let client: LanguageClient | undefined;

  try {
    client = new LanguageClient(
      "tracedecay-selector-test",
      "TraceDecay selector test",
      { command: "unused" },
      { documentSelector: [] },
    );
    const untitledDocument = await vscode.workspace.openTextDocument({
      language: "typescript",
      content: "",
    });

    for (const rootName of rootNames) {
      const rootPath = join(temporaryRoot, rootName);
      const siblingPath = join(temporaryRoot, `${rootName}-sibling`);
      await mkdir(join(rootPath, "nested"), { recursive: true });
      await mkdir(siblingPath, { recursive: true });
      const rootFile = join(rootPath, "root.ts");
      const nestedFile = join(rootPath, "nested", "child.ts");
      const siblingFile = join(siblingPath, "outside.ts");
      await Promise.all(
        [rootFile, nestedFile, siblingFile].map((filePath) =>
          writeFile(filePath, "export {};\n"),
        ),
      );

      const selector: vscode.DocumentSelector = client.protocol2CodeConverter.asDocumentSelector(
        nativeDiagnosticsDocumentSelector(vscode.Uri.file(rootPath).toString()),
      );
      const documents = {
        root: await vscode.workspace.openTextDocument(vscode.Uri.file(rootFile)),
        nested: await vscode.workspace.openTextDocument(vscode.Uri.file(nestedFile)),
        sibling: await vscode.workspace.openTextDocument(
          vscode.Uri.file(siblingFile),
        ),
        untitled: untitledDocument,
      };
      const matches: Record<keyof typeof documents, boolean> = {
        root: vscode.languages.match(selector, documents.root) > 0,
        nested: vscode.languages.match(selector, documents.nested) > 0,
        sibling: vscode.languages.match(selector, documents.sibling) > 0,
        untitled: vscode.languages.match(selector, documents.untitled) > 0,
      };

      assert.deepEqual(
        matches,
        { root: true, nested: true, sibling: false, untitled: false },
        `native diagnostics selector matches for ${JSON.stringify(rootName)}`,
      );
      results.push({
        root: rootName,
        rootDocument: matches.root,
        nestedDocument: matches.nested,
        siblingDocument: matches.sibling,
        untitledDocument: matches.untitled,
      });
    }

    const documentsChecked = results.length * 4;
    const matchedDocuments = results.reduce(
      (count, result) =>
        count + Number(result.rootDocument) + Number(result.nestedDocument),
      0,
    );
    const excludedDocuments = results.reduce(
      (count, result) =>
        count + Number(!result.siblingDocument) + Number(!result.untitledDocument),
      0,
    );

    return {
      rootsChecked: results.length,
      documentsChecked,
      matchedDocuments,
      excludedDocuments,
      results,
    };
  } finally {
    try {
      await client?.dispose();
    } finally {
      await rm(temporaryRoot, { recursive: true, force: true });
    }
  }
}
