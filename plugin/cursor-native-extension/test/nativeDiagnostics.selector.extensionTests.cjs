require("tsx/cjs");

const { runNativeDiagnosticsSelectorHostTest } = require(
  "./nativeDiagnostics.selector.host.ts",
);

exports.run = async () => {
  const result = await runNativeDiagnosticsSelectorHostTest();
  console.log(JSON.stringify(result));
  return result;
};
