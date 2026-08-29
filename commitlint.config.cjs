const allowedTypes = [
  "build",
  "chore",
  "ci",
  "docs",
  "feat",
  "fix",
  "perf",
  "refactor",
  "revert",
  "style",
  "test",
];

module.exports = {
  defaultIgnores: false,
  ignores: [
    (message) => /^Merge[ \t]/.test(message),
    (message) =>
      /^Revert "[^\r\n]+"\r?\n\r?\nThis reverts commit [0-9a-f]{40}\.(?:\r?\n)*$/.test(
        message,
      ),
  ],
  parserPreset: {
    name: "tracedecay",
    parserOpts: {
      headerCorrespondence: ["type", "scope", "breaking", "subject"],
      headerPattern: /^(\w*)(?:\(([A-Za-z0-9._/-]+)\))?(!)?: ([^\s].*)$/,
    },
  },
  rules: {
    "header-max-length": [2, "always", 72],
    "subject-empty": [2, "never"],
    "type-empty": [2, "never"],
    "type-enum": [2, "always", allowedTypes],
  },
};
