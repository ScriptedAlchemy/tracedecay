# tokensave-medium-treesitters

[![Crates.io](https://img.shields.io/crates/v/tokensave-medium-treesitters.svg)](https://crates.io/crates/tokensave-medium-treesitters)
[![docs.rs](https://docs.rs/tokensave-medium-treesitters/badge.svg)](https://docs.rs/tokensave-medium-treesitters)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)

Common tree-sitter grammars for [tokensave](https://github.com/aovestdipaperino/tokensave). This is the **medium** tier, bundling 17 languages (includes all lite-tier languages).

## Languages

### From lite tier

| Language | Grammar Source |
|----------|---------------|
| Rust | [tree-sitter/tree-sitter-rust](https://github.com/tree-sitter/tree-sitter-rust) |
| Python | [tree-sitter/tree-sitter-python](https://github.com/tree-sitter/tree-sitter-python) |
| JavaScript | [tree-sitter/tree-sitter-javascript](https://github.com/tree-sitter/tree-sitter-javascript) |
| TypeScript / TSX | [tree-sitter/tree-sitter-typescript](https://github.com/tree-sitter/tree-sitter-typescript) |
| Go | [tree-sitter/tree-sitter-go](https://github.com/tree-sitter/tree-sitter-go) |
| Java | [tree-sitter/tree-sitter-java](https://github.com/tree-sitter/tree-sitter-java) |
| C | [tree-sitter/tree-sitter-c](https://github.com/tree-sitter/tree-sitter-c) |

### Added in medium tier

| Language | Grammar Source |
|----------|---------------|
| C++ | [tree-sitter/tree-sitter-cpp](https://github.com/tree-sitter/tree-sitter-cpp) |
| C# | [tree-sitter/tree-sitter-c-sharp](https://github.com/tree-sitter/tree-sitter-c-sharp) |
| Ruby | [tree-sitter/tree-sitter-ruby](https://github.com/tree-sitter/tree-sitter-ruby) |
| Kotlin | [fwcd/tree-sitter-kotlin](https://github.com/fwcd/tree-sitter-kotlin) |
| Swift | [tree-sitter/tree-sitter-swift](https://github.com/tree-sitter/tree-sitter-swift) |
| Scala | [tree-sitter/tree-sitter-scala](https://github.com/tree-sitter/tree-sitter-scala) |
| PHP | [tree-sitter/tree-sitter-php](https://github.com/tree-sitter/tree-sitter-php) |
| Bash | [tree-sitter/tree-sitter-bash](https://github.com/tree-sitter/tree-sitter-bash) |
| Lua | [tree-sitter-grammars/tree-sitter-lua](https://github.com/tree-sitter-grammars/tree-sitter-lua) |
| Dart | [UserNobody14/tree-sitter-dart](https://github.com/UserNobody14/tree-sitter-dart) |

## Usage

```rust
use tokensave_medium_treesitters::all_languages;

for (name, language) in all_languages() {
    println!("{name}");
}
```

## Tiers

- **lite** ([tokensave-lite-treesitters](https://crates.io/crates/tokensave-lite-treesitters)) -- 7 languages
- **medium** (this crate) -- 17 languages, includes lite
- **large** ([tokensave-large-treesitters](https://crates.io/crates/tokensave-large-treesitters)) -- 22+ languages, includes medium
