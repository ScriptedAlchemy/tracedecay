# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0-beta.36](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.35...v0.1.0-beta.36) (2026-08-22)


### Features

* **automation:** add crate-owned run label vocabulary ([70ba894](https://github.com/ScriptedAlchemy/tracedecay/commit/70ba8946b1ab3599f215a421fc636d7a49ed9c22))
* **automation:** emit typed tombstone label on skill consolidations ([8cb732e](https://github.com/ScriptedAlchemy/tracedecay/commit/8cb732e84ae26fe6af6792d6f581149eb0d7f516))
* **automation:** wire typed skill-overlap tombstone label into production consolidation records ([4eb0f20](https://github.com/ScriptedAlchemy/tracedecay/commit/4eb0f20465ac4cb8878691a6a8758982eb8f3868))
* **code-index:** resume sealed lexical sources ([da9ead0](https://github.com/ScriptedAlchemy/tracedecay/commit/da9ead09902b73c05e5abffb6cbe262f24cc792d))
* **daemon:** mount durable lexical artifacts ([bc991d1](https://github.com/ScriptedAlchemy/tracedecay/commit/bc991d1473036758d98b8f173f8f92739d20fe89))
* **query:** complete durable lexical artifact cutover ([aa2cc26](https://github.com/ScriptedAlchemy/tracedecay/commit/aa2cc26d20e5edba48a23445e536aa5c4cfcfe42))
* **query:** resume bounded artifact finalization ([f666b6a](https://github.com/ScriptedAlchemy/tracedecay/commit/f666b6a7716e98db4ae8052463ed6a59bd7998cd))
* **retention:** withdraw failed text artifacts ([c87ed0d](https://github.com/ScriptedAlchemy/tracedecay/commit/c87ed0d0e760da363b992d7f4d1aeb08b39d54a3))
* **retention:** zero-copy replay retirement (stage 0b of plan 40) ([0bd09ce](https://github.com/ScriptedAlchemy/tracedecay/commit/0bd09cec5e0bb18455979df37dc3c61df5ab9462))


### Bug Fixes

* **agent-hosts:** admit authored overlap partners ([22dbc1d](https://github.com/ScriptedAlchemy/tracedecay/commit/22dbc1dc9b55f56bb5f9415f752a7890d8de7f93))
* **agent-hosts:** bind overlap tombstones to exact pairs ([ef34f73](https://github.com/ScriptedAlchemy/tracedecay/commit/ef34f73e5826109f19cb3fb0cf733bca17253113))
* **agent-hosts:** bind overlap tombstones to exact pairs ([4abbf4c](https://github.com/ScriptedAlchemy/tracedecay/commit/4abbf4c240bf39fcb63efad3846087dfe967b656))
* **automation:** persist consolidation tombstones atomically ([efa8ea4](https://github.com/ScriptedAlchemy/tracedecay/commit/efa8ea47989d75793e2737e88169840e3c0a01b8))
* **automation:** persist consolidation tombstones atomically ([aae11dd](https://github.com/ScriptedAlchemy/tracedecay/commit/aae11dd51ef6ae6a1f66f74194fcb19c04fb0fcf))
* **automation:** preserve field-specific duration errors ([cd5f9c1](https://github.com/ScriptedAlchemy/tracedecay/commit/cd5f9c1e7e8c58c3c3ca6895da29f30f4aa9fe9b))
* **automation:** return consolidation receipts before sync ([d80cbd3](https://github.com/ScriptedAlchemy/tracedecay/commit/d80cbd34515dccebeaa31f96df4d176005faa836))
* **automation:** validate skill overlap tombstones ([6973717](https://github.com/ScriptedAlchemy/tracedecay/commit/6973717199dde18956e08b11e97a5e466876b918))
* **catalog:** unpin eight existing application-surface operations ([40d72f0](https://github.com/ScriptedAlchemy/tracedecay/commit/40d72f0cc2132350924b4a71f38457d4e645fea5))
* **catalog:** unpin eight existing application-surface operations ([d0697b4](https://github.com/ScriptedAlchemy/tracedecay/commit/d0697b431a87ec89f5891aa4b1d8ca86a748b894))
* **code-index:** charge the sealed decode window ([2c9fe58](https://github.com/ScriptedAlchemy/tracedecay/commit/2c9fe5809289faf3f5483b32b0fd9810ca11ae9e))
* **code-index:** keep cursor offsets internal ([95c7a78](https://github.com/ScriptedAlchemy/tracedecay/commit/95c7a78262a8cb495c0ac59fb70b63021baeea39))
* **code-index:** page large sealed files ([eb0763a](https://github.com/ScriptedAlchemy/tracedecay/commit/eb0763ac4658eda9f02d0ecf70eab9f339335744))
* **code-index:** serialize text artifact publication ([15de2a7](https://github.com/ScriptedAlchemy/tracedecay/commit/15de2a71bda55800e184fff66650813735aba4e8))
* **daemon:** alias observability producers per exact store authority ([8b11a11](https://github.com/ScriptedAlchemy/tracedecay/commit/8b11a11be2ca51a00dba846589f9dbadf4055f71))
* **daemon:** alias observability producers per exact store authority ([4418102](https://github.com/ScriptedAlchemy/tracedecay/commit/44181022b0b4d946963fdaaa837219beb43f9cb8))
* **daemon:** bound lexical artifact activation ([48a4977](https://github.com/ScriptedAlchemy/tracedecay/commit/48a49771196bab648991de10027325ed0c02a5b8))
* **daemon:** keep runtimeless drops retiring until the drain confirms ([4757f6a](https://github.com/ScriptedAlchemy/tracedecay/commit/4757f6a129645deffd7645e42f126ca47a5e902e))
* **daemon:** keep runtimeless drops retiring until the drain confirms ([f61afbd](https://github.com/ScriptedAlchemy/tracedecay/commit/f61afbd024523ad5b6a596d6f15455c267628474))
* **daemon:** key observability aliases by store authority ([5a44002](https://github.com/ScriptedAlchemy/tracedecay/commit/5a44002287078ef75f87d6700d6650d65c29127c))
* **daemon:** preserve linked-root observability policy ([e62a29f](https://github.com/ScriptedAlchemy/tracedecay/commit/e62a29fc6598a77d073ec8411df44b8d8ef84a96))
* **daemon:** preserve linked-root observability policy ([eda2929](https://github.com/ScriptedAlchemy/tracedecay/commit/eda292906bb09a66e32bd4c09ede241fa0c8277a))
* **daemon:** preserve text build interruptions ([084d5f3](https://github.com/ScriptedAlchemy/tracedecay/commit/084d5f309f9cc5314ae94bbd0aef29126d4b8101))
* **daemon:** rebuild incompatible lexical artifacts ([f05a7c7](https://github.com/ScriptedAlchemy/tracedecay/commit/f05a7c767dc28391f5261cd620c87f9d65126329))
* **daemon:** rebuild unavailable text artifacts ([1aeff5f](https://github.com/ScriptedAlchemy/tracedecay/commit/1aeff5f13fd6c5c85303bd37a4d8e63b6708342f))
* **daemon:** retain alias observability identity ([381a4d8](https://github.com/ScriptedAlchemy/tracedecay/commit/381a4d817f32e5d56ee949c9d8549d7da0b36cb4))
* **daemon:** retain observability owners through shutdown ([496bd52](https://github.com/ScriptedAlchemy/tracedecay/commit/496bd52efcef8018930d3f48e7a839e9705300e8))
* **global-db:** align refusal coverage with cursor authority ([aa11fd2](https://github.com/ScriptedAlchemy/tracedecay/commit/aa11fd20a31b30b0eea094cb1bf15a40f25b4ef2))
* **global-db:** clear admission refusals in the scoped authority reset ([8a2849d](https://github.com/ScriptedAlchemy/tracedecay/commit/8a2849db732eb681c8d1d652b71b2afc4ed9de7c))
* **global-db:** converge only different-output provenance collisions ([5e32f13](https://github.com/ScriptedAlchemy/tracedecay/commit/5e32f131d20e4c9b4f32fab3200cfed70c342587))
* **global-db:** converge refusal coverage on fast-path re-admits ([211b968](https://github.com/ScriptedAlchemy/tracedecay/commit/211b968b911a85d94d2e7f91683a53340aa9a406))
* **global-db:** durable terminal coverage for observation collisions ([4868b1b](https://github.com/ScriptedAlchemy/tracedecay/commit/4868b1b997b01309ba0b4d033854c272bfbf4f84))
* **global-db:** enforce retention scope authority ([fddb6d5](https://github.com/ScriptedAlchemy/tracedecay/commit/fddb6d5c41746c4da470f5389211d4e712c21c3f))
* **global-db:** enforce retention scope authority ([21ba4b7](https://github.com/ScriptedAlchemy/tracedecay/commit/21ba4b712776b978b62f21bfa0142cb37f1eaf7e))
* **global-db:** make collision refusal progress durable ([9adad7a](https://github.com/ScriptedAlchemy/tracedecay/commit/9adad7af0377221d4cbb7056e1b9696deff79b39))
* **global-db:** make observation collisions terminal ([236df00](https://github.com/ScriptedAlchemy/tracedecay/commit/236df00cca6c31bfe2c4942b8d962349c8362c9c))
* **global-db:** record refusal marker and coverage atomically ([e198d80](https://github.com/ScriptedAlchemy/tracedecay/commit/e198d80781c2b5046199e01341fd8300a3bbdbbb))
* **global-db:** surface refusal frontier advance construction errors ([cef7761](https://github.com/ScriptedAlchemy/tracedecay/commit/cef77619f7d107175c46077fc7d8a36f2775104a))
* **global-db:** treat source generations as opaque identities ([bd92565](https://github.com/ScriptedAlchemy/tracedecay/commit/bd92565c3f1930cb7467546bd292d06c9165125b))
* **graph:** enforce private replay root ([3bc6e1a](https://github.com/ScriptedAlchemy/tracedecay/commit/3bc6e1a5279d7efeb8ddc15a594024e3ff98210e))
* **graph:** preserve replay collision error contract ([3ea49da](https://github.com/ScriptedAlchemy/tracedecay/commit/3ea49da7dba79abcba4c0225f629d840f655858a))
* **graph:** preserve staged replay unlink identity ([dccb483](https://github.com/ScriptedAlchemy/tracedecay/commit/dccb48329cd346198e2a6803113655c8a1fded34))
* **graph:** verify lazy replay sources ([d46b55d](https://github.com/ScriptedAlchemy/tracedecay/commit/d46b55dc5eabc76a59fa42945677e87fdb04edfe))
* **observability:** replay durable policy across restarts ([bf46b23](https://github.com/ScriptedAlchemy/tracedecay/commit/bf46b23eab417e35dbdf96b96d608d3707040530))
* **query:** bind artifact reader to verified file ([56ed654](https://github.com/ScriptedAlchemy/tracedecay/commit/56ed654f7f8a3ec67b7f4f93b1a634ddd4e04cb6))
* **query:** bound document integrity verification ([5904329](https://github.com/ScriptedAlchemy/tracedecay/commit/59043290f0797e57bbba185a30af8e50fc3a4a01))
* **query:** bound document integrity verification ([5a8bf7c](https://github.com/ScriptedAlchemy/tracedecay/commit/5a8bf7c295eec094307ea2202ca3d29550b2222b))
* **query:** bound finalization result size ([a31c5ba](https://github.com/ScriptedAlchemy/tracedecay/commit/a31c5ba258037cae6d4ab575227ccea5d4422f3f))
* **query:** conservatively charge n-gram scratch ([6acd07d](https://github.com/ScriptedAlchemy/tracedecay/commit/6acd07d595e17412da259941c8f7c6d33d8bbf0b))
* **query:** deduplicate streamed lexical documents ([b2af77d](https://github.com/ScriptedAlchemy/tracedecay/commit/b2af77db8acc3ce886973fec1fae2857d07799d3))
* **query:** flatten lexical ngram intersections ([eb4df8d](https://github.com/ScriptedAlchemy/tracedecay/commit/eb4df8dc1522e7e2d6604cafe8a467ecf45f0788))
* **query:** harden lexical artifact opens ([e2cdbe1](https://github.com/ScriptedAlchemy/tracedecay/commit/e2cdbe18802dd8a3f2450157d24c0adee0d1d2c3))
* **query:** honest artifact memory ledger and bounded top-K reads (B2) ([bb372f6](https://github.com/ScriptedAlchemy/tracedecay/commit/bb372f6bdce413201ae87104225d1f4020b6c758))
* **query:** make artifact sealing fail closed ([9a5b83e](https://github.com/ScriptedAlchemy/tracedecay/commit/9a5b83ea9e0d814f8f33d3aeb29ee42f146f1a3c))
* **query:** retain bounded artifact cache accounting ([1f3205d](https://github.com/ScriptedAlchemy/tracedecay/commit/1f3205d5015975262ab3a187e1ece95ec7954e62))
* **query:** seek native keys during artifact finalization ([57e2141](https://github.com/ScriptedAlchemy/tracedecay/commit/57e21415e1e16a485430e6885a25fdaa9663591e))
* **query:** stream lexical artifact candidates ([7799c9e](https://github.com/ScriptedAlchemy/tracedecay/commit/7799c9ea3b01ae1374caffb29872a578c68dace7))
* **query:** type finalization accumulator ([f012622](https://github.com/ScriptedAlchemy/tracedecay/commit/f012622a7c9c85a853e7b01cd1ab084fe991644b))
* **query:** verify artifact identity across platforms ([34cd15a](https://github.com/ScriptedAlchemy/tracedecay/commit/34cd15a9615286956a31815574f1249924bcb943))
* **query:** verify durable lexical artifact handle ([29641b8](https://github.com/ScriptedAlchemy/tracedecay/commit/29641b8ef07691faebbb6c466821772191c2d249))
* **query:** verify staged artifacts before sealing ([4de90e8](https://github.com/ScriptedAlchemy/tracedecay/commit/4de90e8a0afafb67d766404ccf6f5c983b93181c))
* **retention:** cancel artifact apply verification ([231ce35](https://github.com/ScriptedAlchemy/tracedecay/commit/231ce355a6c68784222746d178f5205202652ddc))
* **retention:** expose artifact inventory diagnostics ([dbd5163](https://github.com/ScriptedAlchemy/tracedecay/commit/dbd516301528bb2f9b7c596149150445580e43ea))
* **retention:** harden graph replay retirement ([63f0426](https://github.com/ScriptedAlchemy/tracedecay/commit/63f0426999b964af77e6317b1e10830c3a82a044))
* **retention:** mount cancellation through apply ([5aadb57](https://github.com/ScriptedAlchemy/tracedecay/commit/5aadb57f68cda71728c0c928606e13f1b42371b5))
* **retention:** reclaim orphan text artifacts safely ([418504b](https://github.com/ScriptedAlchemy/tracedecay/commit/418504b2835329af0f0354b3cd0ea99fcb0addc7))
* **retention:** run artifact-only maintenance ([9725875](https://github.com/ScriptedAlchemy/tracedecay/commit/9725875a198d47be2eb953220e2041f18bbeb6c2))
* **runtime-core:** bind test runtimes to profile identity ([39137d1](https://github.com/ScriptedAlchemy/tracedecay/commit/39137d18044805c412a2d3b1273da134c3c3012a))
* **runtime-core:** bind test runtimes to profile identity ([626369a](https://github.com/ScriptedAlchemy/tracedecay/commit/626369a95aa87264682e115878f777d98fbaa46c))
* **runtime-core:** derive test runtime identity per fixture root ([5d1d70e](https://github.com/ScriptedAlchemy/tracedecay/commit/5d1d70e3ae8fb663c8a4d5e8567f373fb519871e))
* **storage:** keep schema installation authority-bound ([3c26c02](https://github.com/ScriptedAlchemy/tracedecay/commit/3c26c029e808c87759ef29bc8791f1dbddd17b7d))
* **storage:** keep schema installation authority-bound ([bcdbf82](https://github.com/ScriptedAlchemy/tracedecay/commit/bcdbf824e6af211b66369c6299568543f3c475cf))


### Performance Improvements

* **code-index:** bound sealed source layout memory ([b07a63c](https://github.com/ScriptedAlchemy/tracedecay/commit/b07a63c5d36e7bf363d93ff2c0a686933377a0d7))
* **daemon:** batch artifact finalization rows ([ffd0c5f](https://github.com/ScriptedAlchemy/tracedecay/commit/ffd0c5fc3c8c7fbcee75d1238e8de27be91e4c9b))
* **graph:** stop eager replay seal copies ([d13a013](https://github.com/ScriptedAlchemy/tracedecay/commit/d13a0135320919295910572fac86457c26dbbe26))
* **graph:** stop eager replay seal copies ([b2a0577](https://github.com/ScriptedAlchemy/tracedecay/commit/b2a05773313d1621f132a05d9e19bd63d4535078))
* **retention:** keep artifact census metadata-only ([90074fe](https://github.com/ScriptedAlchemy/tracedecay/commit/90074feabe4abc6f87a4ab5fb81261c536e25cee))

## [0.1.0-beta.35](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.34...v0.1.0-beta.35) (2026-08-22)


### Bug Fixes

* **benches:** wrap manifests for verified graph publish ([a519d85](https://github.com/ScriptedAlchemy/tracedecay/commit/a519d850606e40c80a513f41292617d86f692e2e))
* **code-index:** wrap bench manifest for verified publish ([9c16732](https://github.com/ScriptedAlchemy/tracedecay/commit/9c16732d6dce7c5c1f4ff3f1727ad925f0613f19))
* **context:** preserve git evidence during graph warmup ([a1ae71b](https://github.com/ScriptedAlchemy/tracedecay/commit/a1ae71b9dc49c2d873b166d4e417fa5205488ddb))
* **context:** propagate terminal graph failures ([db76b58](https://github.com/ScriptedAlchemy/tracedecay/commit/db76b5858ae94295a421070812d134b1510455e8))
* **graph-db:** wrap bench manifest for verified publish ([f191c77](https://github.com/ScriptedAlchemy/tracedecay/commit/f191c77d5337164c8f56ece7962fd1aeacf5d580))
* **graph-db:** wrap mismatch bench manifests for verified publish ([4c9c06a](https://github.com/ScriptedAlchemy/tracedecay/commit/4c9c06a6ae1192259722e60e2bc21004c8def950))
* **release:** detect aliased feature owners ([2b88657](https://github.com/ScriptedAlchemy/tracedecay/commit/2b886570804550e041ca47b27c18f796613e5e7d))
* **release:** reject shadow feature ownership ([d657644](https://github.com/ScriptedAlchemy/tracedecay/commit/d6576445e3151f4ec832e026898b4856502f4957))
* **release:** stream release metadata validation ([a362e21](https://github.com/ScriptedAlchemy/tracedecay/commit/a362e21f0391d19696d4148818fd14b7bcf38bcc))
* **release:** validate exact published prerelease ([583f8a9](https://github.com/ScriptedAlchemy/tracedecay/commit/583f8a95724f8e9f39181bf7b0eaa7b93406dd61))
* **release:** validate exact published prerelease ([ed1eadc](https://github.com/ScriptedAlchemy/tracedecay/commit/ed1eadc4aca33eea2ca89ae2eb784e915c5e8d31))
* **release:** validate semantic feature owner ([1e62bb2](https://github.com/ScriptedAlchemy/tracedecay/commit/1e62bb2f65c3f8e7a4f13c7ce92cdaeeda72e098))
* **release:** validate semantic feature owner ([0eaeed3](https://github.com/ScriptedAlchemy/tracedecay/commit/0eaeed38b508fa1cf7fd30525694709fefd8f0c3))
* **rusqlite-runtime:** move lease_clock module below production items ([b216304](https://github.com/ScriptedAlchemy/tracedecay/commit/b2163041ace6fdbedd0f8dce0dc7eff10a7061da))
* **rusqlite-runtime:** move lease_clock test module below production items ([4adb71f](https://github.com/ScriptedAlchemy/tracedecay/commit/4adb71fe20d01e74855771dc726e8631459ae140))
* **tests:** satisfy workspace clippy gates ([5c34626](https://github.com/ScriptedAlchemy/tracedecay/commit/5c34626e932f08d47823fc5b88c38502f2d1e66a))
* **tests:** satisfy workspace Clippy gates ([59f7875](https://github.com/ScriptedAlchemy/tracedecay/commit/59f7875414c5a29ed1029b823627af5fe7d20a47))

## [0.1.0-beta.34](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.33...v0.1.0-beta.34) (2026-08-22)


### Bug Fixes

* **graph:** resolve sealed replays from the canonical generation root ([5ddd162](https://github.com/ScriptedAlchemy/tracedecay/commit/5ddd16271e8ee288e3973ecd914e84d5d762a93f))

## [0.1.0-beta.33](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.32...v0.1.0-beta.33) (2026-08-21)


### Bug Fixes

* **build:** restore root lint gate ([c25e8a1](https://github.com/ScriptedAlchemy/tracedecay/commit/c25e8a1d99b11a2051c7fcbe5eb48da2866a6c93))
* **code-index:** restore lexical source lint gates ([484510c](https://github.com/ScriptedAlchemy/tracedecay/commit/484510c7449894bb49884a4b6b7cadaa68b033df))
* **context:** preserve fallback results during warmup ([89cbecc](https://github.com/ScriptedAlchemy/tracedecay/commit/89cbecc232cbcfa007709afa81433ee0b3a3d821))
* **query:** right-size lexical ngram pages ([cdfe2ea](https://github.com/ScriptedAlchemy/tracedecay/commit/cdfe2ea97bcef3780085d6c0059be4d11025e70c))
* **runtime:** preserve context during index warmup ([da6eb98](https://github.com/ScriptedAlchemy/tracedecay/commit/da6eb987f0b71651745fe9e9481f2d0226001514))

## [0.1.0-beta.32](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.31...v0.1.0-beta.32) (2026-08-21)


### Features

* **query:** add bounded lexical artifacts ([781657e](https://github.com/ScriptedAlchemy/tracedecay/commit/781657e2a5d3d994a0b53e6e200f354509225d02))

## [0.1.0-beta.31](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.30...v0.1.0-beta.31) (2026-08-21)


### Features

* **code-index:** stream verified import evidence ([c11cb0d](https://github.com/ScriptedAlchemy/tracedecay/commit/c11cb0da20f0e5405dbe0d9782e353488430f09f))

## [0.1.0-beta.30](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.29...v0.1.0-beta.30) (2026-08-21)


### Features

* **code-index:** stream verified lexical pages ([975caf8](https://github.com/ScriptedAlchemy/tracedecay/commit/975caf8c7533deb16d6a2540f392882989c1865b))


### Bug Fixes

* **projection:** converge predecessor before host drains ([beb69da](https://github.com/ScriptedAlchemy/tracedecay/commit/beb69dabb6ab743af4476fef65e7f339ee399f6e))


### Performance Improvements

* **query:** bound ngram posting storage ([fe23b94](https://github.com/ScriptedAlchemy/tracedecay/commit/fe23b943a05054faba9becad8236d0f97496e754))

## [0.1.0-beta.29](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.28...v0.1.0-beta.29) (2026-08-21)


### Features

* **agent-hosts:** auto-trust Codex managed hooks on install and update ([dbed6d3](https://github.com/ScriptedAlchemy/tracedecay/commit/dbed6d3f18d7a9651d6f790db000088de79d3680))
* **agent-hosts:** auto-trust Codex managed hooks on install and update ([83be52d](https://github.com/ScriptedAlchemy/tracedecay/commit/83be52d382d6f14219063b6a681460de964a8926))
* **automation:** make session-evidence budget backoff window configurable ([1524f03](https://github.com/ScriptedAlchemy/tracedecay/commit/1524f032cca7ac9661a25f721f39acd166563e4d))
* **automation:** typed session-evidence budget backoff for scheduler skips ([dd5b677](https://github.com/ScriptedAlchemy/tracedecay/commit/dd5b6779c770cdd6e25240e35fd3d5c9a1bd8552))
* **code-index:** configure native graph activation ([4be1897](https://github.com/ScriptedAlchemy/tracedecay/commit/4be1897dd9809bf60e7e950f989b410c2c478b0c))
* mount privacy remediation, multi-root dashboard scope, native-integration notifications ([70182de](https://github.com/ScriptedAlchemy/tracedecay/commit/70182de3eae04a3cf81b03d6b2274de8fc0536cb))
* **privacy:** rescan at-rest LCM store bytes under current detector ([1c58d39](https://github.com/ScriptedAlchemy/tracedecay/commit/1c58d39caf5311ff30bb121cdc1e71ec68764632))
* **privacy:** rescan at-rest LCM store bytes under the current detector ([327011a](https://github.com/ScriptedAlchemy/tracedecay/commit/327011a7460f2ec220b50b96ead256e85f87c097))
* **private-fs:** mount quota-aware available_space ([747999e](https://github.com/ScriptedAlchemy/tracedecay/commit/747999e9164de414916e0652af4ea1bc874b8f2a))
* **private-fs:** mount quota-aware available_space ([1edb427](https://github.com/ScriptedAlchemy/tracedecay/commit/1edb42763004990b019cd7f57fcccebd517864a4))
* **rusqlite-runtime:** expose exclusive maintenance checkpoint port ([f8de48d](https://github.com/ScriptedAlchemy/tracedecay/commit/f8de48d0c9ef50f2f680c6b3eaf2a1f7b2504152))
* **rusqlite-runtime:** expose exclusive maintenance checkpoint port ([6eb16dd](https://github.com/ScriptedAlchemy/tracedecay/commit/6eb16dd6dd3c3c5808218f34d364dc2f84ba9ae1))
* **rusqlite-runtime:** expose exclusive maintenance checkpoint port ([a13a7f4](https://github.com/ScriptedAlchemy/tracedecay/commit/a13a7f463f47b35eb79e7d08d85b29889aec766b))
* **scout:** remount the Context Scout producer path and Plan 20 flag ([ab6099e](https://github.com/ScriptedAlchemy/tracedecay/commit/ab6099e45651a249d7699cb2300cd8764a0cea66))


### Bug Fixes

* **agent-hosts:** adopt receiptless cataloged deploys on install/update ([8c82f0d](https://github.com/ScriptedAlchemy/tracedecay/commit/8c82f0dd641448f64ac65a566ac72d79cb767316))
* **agent-hosts:** adopt receiptless cataloged deploys on install/update ([3ecb5a0](https://github.com/ScriptedAlchemy/tracedecay/commit/3ecb5a0bd4047a6812043e4acf7b6fa1ccef7aef))
* **agent-hosts:** gate receiptless adoption on explicit authority ([1044ce5](https://github.com/ScriptedAlchemy/tracedecay/commit/1044ce5a7602027157488d50f5d43ec5eac90b75))
* **agent-hosts:** gate receiptless adoption on explicit authority ([6b07061](https://github.com/ScriptedAlchemy/tracedecay/commit/6b07061ad26376437f058a95a8c399b3ab71ec36))
* **agent-hosts:** grant Claude plugin wildcard ([fbf65bf](https://github.com/ScriptedAlchemy/tracedecay/commit/fbf65bf6f2eceb577606979d8dea53f4e21914a7))
* **agent-hosts:** preserve multicall interpreter name ([1f50ffd](https://github.com/ScriptedAlchemy/tracedecay/commit/1f50ffdbaf9808d1f951a51f0c18eff1eea9e383))
* **agent-hosts:** preserve original mode on host config backups ([f4fbdcb](https://github.com/ScriptedAlchemy/tracedecay/commit/f4fbdcb078b7aea7126490ca813a1da31c14f6dd))
* **agent-hosts:** recover versioned registration paths ([bda88ee](https://github.com/ScriptedAlchemy/tracedecay/commit/bda88ee2825694a1eedc256e51e8dfd6c043630f))
* **agent-hosts:** report usable ownership recovery ([9c7ae7d](https://github.com/ScriptedAlchemy/tracedecay/commit/9c7ae7d27823031f41763301fe4faa28c0794dd0))
* **agent-hosts:** require explicit receiptless adoption ([8ec75e8](https://github.com/ScriptedAlchemy/tracedecay/commit/8ec75e8c9328cfda3b5fb1360b4f3431c17e3b78))
* **agent-hosts:** retain profile memory graph port in user harness ([ef23c7c](https://github.com/ScriptedAlchemy/tracedecay/commit/ef23c7c153e0a470e6bee3c1f5f8aa09a68f9ded))
* **agent-hosts:** sync Codex hook trust in activation, prune on removal ([21fdaa0](https://github.com/ScriptedAlchemy/tracedecay/commit/21fdaa073d8a32067a33ad976f9576eacd38c06c))
* **application:** bind session evidence budget suppressed label ([320464f](https://github.com/ScriptedAlchemy/tracedecay/commit/320464f2bc5101f2582d37f4c70eb9fdfc1d2697))
* **application:** return typed work-catalog identity failures ([0398bb4](https://github.com/ScriptedAlchemy/tracedecay/commit/0398bb4081db1eb32c90d9587590a31152978179))
* **automation:** dedicate suppressed-tick skip reason and reject zero window ([fbf12e2](https://github.com/ScriptedAlchemy/tracedecay/commit/fbf12e2076739cd62f2d4f198ffc0279279352d4))
* **automation:** distinguish budget backoff skips ([d775002](https://github.com/ScriptedAlchemy/tracedecay/commit/d775002d9f5b935f5a732d7d234b10bfe0a5f15f))
* **automation:** preserve budget backoff ordering ([18f1237](https://github.com/ScriptedAlchemy/tracedecay/commit/18f1237cd6572838f13ebb010743692492c98b14))
* **automation:** share evidence budget labels ([7002612](https://github.com/ScriptedAlchemy/tracedecay/commit/70026120a4967e1db541a486829066cf32e8c2e5))
* **build:** preserve lean feature compilation ([060046f](https://github.com/ScriptedAlchemy/tracedecay/commit/060046fc1bd2e9f554d17d49114cdd239b6300de))
* **build:** satisfy Rust 1.97 Clippy policy ([4926a59](https://github.com/ScriptedAlchemy/tracedecay/commit/4926a5939390bb721fe1e6ed5a5318be654d88da))
* **claude:** separate row and message identities ([b785e63](https://github.com/ScriptedAlchemy/tracedecay/commit/b785e6327acefbfa8ccda55fbb994b699a0067ec))
* **cli:** carry the status deadline into daemon tool calls ([dfa752a](https://github.com/ScriptedAlchemy/tracedecay/commit/dfa752a31f79b8edfbf7d975cbacc5797efac401))
* **cli:** classify read-only startup policy ([bcd937a](https://github.com/ScriptedAlchemy/tracedecay/commit/bcd937a5943cfd72f0b4d0c55127a97eff4c727f))
* **cli:** forward receiptless adoption authority ([e2b173e](https://github.com/ScriptedAlchemy/tracedecay/commit/e2b173e8487721178c94848a9cbbef37e9182a5e))
* **cli:** prevent implicit host repairs ([a762092](https://github.com/ScriptedAlchemy/tracedecay/commit/a76209206e586391998e158ee6f227a0625db451))
* **cli:** request the code-index reconcile init reports ([2e066ad](https://github.com/ScriptedAlchemy/tracedecay/commit/2e066ad0f92f934cf14e5ce294158c1a453ad2fe))
* **cli:** require confirmed host adoption ([a275cc5](https://github.com/ScriptedAlchemy/tracedecay/commit/a275cc5b0c63ca2b8df5cde4209d6bb4dd1d2481))
* **cli:** skip install checks for nested inspections ([89c1951](https://github.com/ScriptedAlchemy/tracedecay/commit/89c195195146808ff75a33ea7e330c7a2e21be49))
* **cli:** update explicit reinstall test imports ([6424fd0](https://github.com/ScriptedAlchemy/tracedecay/commit/6424fd02f36dfd1570f69e5173516bdce78c58b5))
* **code-index:** code-index leftover slice for [#421](https://github.com/ScriptedAlchemy/tracedecay/issues/421) — full-scope dispatch, sealed-branch shard key ([fa37dd9](https://github.com/ScriptedAlchemy/tracedecay/commit/fa37dd9779bb31a3d87d8692ba4885436f7f10aa))
* **code-index:** compile graph policy without test authority ([2dbc2ea](https://github.com/ScriptedAlchemy/tracedecay/commit/2dbc2ead8815fe55d29440e50c4c73e06ca886e0))
* **code-index:** dispatch active generations by exact full scope ([49f8acc](https://github.com/ScriptedAlchemy/tracedecay/commit/49f8acc7c097bb2a68b03ce745c9d58d78112bfe))
* **code-index:** fail closed on late ceiling install ([63b012c](https://github.com/ScriptedAlchemy/tracedecay/commit/63b012ccaf3496532927edade960f364f57e02de))
* **code-index:** honor daemon worker ceiling ([95819cb](https://github.com/ScriptedAlchemy/tracedecay/commit/95819cbc70d99cb4cdc49bd3df258a08e41ce34e))
* **code-index:** honor daemon worker ceiling ([bf8f4fd](https://github.com/ScriptedAlchemy/tracedecay/commit/bf8f4fd15e9196364a8352a97bf1324761a10e3e))
* **code-index:** reclaim graph manifest memo ([0dcdd8f](https://github.com/ScriptedAlchemy/tracedecay/commit/0dcdd8f09df6e6bc550594fe9c4e771950473d65))
* **code-index:** refuse graph before retained hydration ([70d091e](https://github.com/ScriptedAlchemy/tracedecay/commit/70d091e7995b92cd79a2b08375099321f1854219))
* **code-index:** serialize worker ceiling setup ([3b371c6](https://github.com/ScriptedAlchemy/tracedecay/commit/3b371c6490dca83728a0d46a8daf9378f1b8a3fc))
* **code-index:** serve text when graph budget is refused ([10745a1](https://github.com/ScriptedAlchemy/tracedecay/commit/10745a1e2e109f30b2226c9d24cb1e159ead0cd7))
* **code-index:** update graph policy on remount ([99c50aa](https://github.com/ScriptedAlchemy/tracedecay/commit/99c50aa3c6e9a9d79630fb531eb56f1c6c6a050d))
* **codex:** bind stable observation source context ([88ad214](https://github.com/ScriptedAlchemy/tracedecay/commit/88ad214316344be19018ed5f3ec78fdd97484851))
* **config:** converge native graph setting on existing stores ([8c74c58](https://github.com/ScriptedAlchemy/tracedecay/commit/8c74c58dac875f8a03e34be4f69652e676307785))
* **cursor:** stabilize canonical observation context ([cf4868a](https://github.com/ScriptedAlchemy/tracedecay/commit/cf4868acf9cb0f0df38c9702abdd359ec9755195))
* **daemon:** bound shutdown coordinator receipts ([7c11144](https://github.com/ScriptedAlchemy/tracedecay/commit/7c11144364f8a248fad4f55d2a9598cddff7be8f))
* **daemon:** defer slow repository discovery instead of failing open ([39bbac1](https://github.com/ScriptedAlchemy/tracedecay/commit/39bbac1180d5152f30f48f0e9d67ec08117724e3))
* **daemon:** drain terminal graph owner states ([2f7239e](https://github.com/ScriptedAlchemy/tracedecay/commit/2f7239e5b7e4ed38ac49ab7d3aea93c641781bee))
* **daemon:** drain terminal graph owner states ([a0310e6](https://github.com/ScriptedAlchemy/tracedecay/commit/a0310e6a8b3a8eadd99a51be812d0ff51c6f748f))
* **daemon:** harden first-index recovery ([a4e47ba](https://github.com/ScriptedAlchemy/tracedecay/commit/a4e47ba0964ed4326d17f3a08a7b8dfc6ad85cd3))
* **daemon:** harden project privacy and status routing ([f1655c8](https://github.com/ScriptedAlchemy/tracedecay/commit/f1655c8ae3efc44c229f7ee7eab760a1aec57558))
* **daemon:** honor graph policy for linked worktrees ([788d824](https://github.com/ScriptedAlchemy/tracedecay/commit/788d824d0ed8d9bf1455d4f8c89a663a57166f58))
* **daemon:** join active work attempt processes ([9f18046](https://github.com/ScriptedAlchemy/tracedecay/commit/9f1804681716825a0bc0dd4ebf58b89a7d2b62a4))
* **daemon:** join active work attempt processes ([f221dc7](https://github.com/ScriptedAlchemy/tracedecay/commit/f221dc776d04053324e1e25671baa83f0fefb327))
* **daemon:** join bounded hook cycle workers ([fcb5443](https://github.com/ScriptedAlchemy/tracedecay/commit/fcb5443d5b236816ec2c5814a1df742a014d4c12))
* **daemon:** join bounded hook cycle workers ([1f9e429](https://github.com/ScriptedAlchemy/tracedecay/commit/1f9e429bc7ee04cee0ea3a5fdc5f42563afbb9a1))
* **daemon:** join project-open background leases ([a8503ee](https://github.com/ScriptedAlchemy/tracedecay/commit/a8503ee761789314afae49178c052e86c35de06e))
* **daemon:** join reconciliation workers before closing retained graphs ([59b3760](https://github.com/ScriptedAlchemy/tracedecay/commit/59b37600047d2abc5909275986c1f00eb85055c6))
* **daemon:** join session runtime terminal tasks ([4ba8be4](https://github.com/ScriptedAlchemy/tracedecay/commit/4ba8be4fbf2e76bcd58a3ed6b02690555d8786af))
* **daemon:** keep committed-profile unseated retention retryable ([f45e856](https://github.com/ScriptedAlchemy/tracedecay/commit/f45e85641d3096d9d96793e080d9234b60d6c70e))
* **daemon:** keep the unix socket endpoint within SUN_LEN ([a64e7d6](https://github.com/ScriptedAlchemy/tracedecay/commit/a64e7d6a142d51d88b2582143538a27db0e11b0b))
* **daemon:** latch retirement reaper shutdown wakes ([2567125](https://github.com/ScriptedAlchemy/tracedecay/commit/2567125af033165b3096ee35f69686f660be2ca4))
* **daemon:** mount budget-aware lexical projection build ([906eea1](https://github.com/ScriptedAlchemy/tracedecay/commit/906eea12c5c299b1f57a311c77c9539e9d72ec20))
* **daemon:** preserve exact branch ref identity ([7a721df](https://github.com/ScriptedAlchemy/tracedecay/commit/7a721df17d8ed3d2cf2d94996fb47db37f4fef26))
* **daemon:** preserve exact branch ref identity ([6724e6b](https://github.com/ScriptedAlchemy/tracedecay/commit/6724e6bc2644e230154642f74b4585cd2d58359a))
* **daemon:** preserve semantic retries on no-op ([4467464](https://github.com/ScriptedAlchemy/tracedecay/commit/4467464e28d1baca206b42f36fa24f4b4d84527b))
* **daemon:** propagate terminal runtime shutdown failures ([b7ed3eb](https://github.com/ScriptedAlchemy/tracedecay/commit/b7ed3eb4290ba2d65b81d1001f145024bb8f0bc2))
* **daemon:** quiet vector retention when semantic runtime is unseated ([78509af](https://github.com/ScriptedAlchemy/tracedecay/commit/78509afaccbb21aa99459788a344854ba0c40e0f))
* **daemon:** quiet vector retention when semantic runtime is unseated ([ae3d107](https://github.com/ScriptedAlchemy/tracedecay/commit/ae3d107cb9a1b1d6c5f85ee7ba8c03d815fa44b3))
* **daemon:** release session graph leases at shutdown ([c3907ff](https://github.com/ScriptedAlchemy/tracedecay/commit/c3907ffbe0f99818842966e4e0ea411c965b915d))
* **daemon:** release session graph leases at shutdown ([b6001dd](https://github.com/ScriptedAlchemy/tracedecay/commit/b6001ddf8d1b7cd10934b54360332aa7710d21cd))
* **daemon:** release session sync stores at shutdown ([3a1b00a](https://github.com/ScriptedAlchemy/tracedecay/commit/3a1b00af3cfd73430366975fba8792bc446b080e))
* **daemon:** release session sync stores at shutdown ([d2816ed](https://github.com/ScriptedAlchemy/tracedecay/commit/d2816ed54899a775d0be6e56ce6b1d0e59228a48))
* **daemon:** release telemetry clients at shutdown ([725e30b](https://github.com/ScriptedAlchemy/tracedecay/commit/725e30b59920a783fe79179fc64455768833f986))
* **daemon:** remount scout producer path on settings revision drift ([d630d1e](https://github.com/ScriptedAlchemy/tracedecay/commit/d630d1e458366eb699cb8659c719dff5da55b3fe))
* **daemon:** reserve scheduler retirement after admission ([41fe56b](https://github.com/ScriptedAlchemy/tracedecay/commit/41fe56b0069e761668de86fb19c500bbb0d08863))
* **daemon:** retain session sync tasks through shutdown ([04da6df](https://github.com/ScriptedAlchemy/tracedecay/commit/04da6dfc9015bcba31bba5741792f8b52cfc2c7e))
* **daemon:** retain timed-out shutdown runner ownership ([c605133](https://github.com/ScriptedAlchemy/tracedecay/commit/c6051338c4319e4078debe20b9896bcf2b3f2b9a))
* **daemon:** Scout P1 follow-up after [#521](https://github.com/ScriptedAlchemy/tracedecay/issues/521) ([d6a273c](https://github.com/ScriptedAlchemy/tracedecay/commit/d6a273c94317274fe3f66d7926b8c38a1e976bf6))
* **daemon:** skip redundant graph activation ([1678492](https://github.com/ScriptedAlchemy/tracedecay/commit/167849236b4515871360c5fecd5d16b17cc981ee))
* **daemon:** skip redundant graph activation ([214781f](https://github.com/ScriptedAlchemy/tracedecay/commit/214781f4e9195f7c1352fbd6f40afe0a020200e0))
* **daemon:** stable first-index on empty profiles (discovery deferral, real init reconcile, accept/SUN_LEN hardening) ([5259f5b](https://github.com/ScriptedAlchemy/tracedecay/commit/5259f5b03f24b73c1d27587291ee2f9399adb185))
* **daemon:** survive transient accept errors instead of exiting ([ea0eda9](https://github.com/ScriptedAlchemy/tracedecay/commit/ea0eda9ec7b5795ab8517d0fc5814fe710f285f5))
* **global-db:** restore measured WAL reclaim to registered checkpoint ([28135a5](https://github.com/ScriptedAlchemy/tracedecay/commit/28135a5dc50976eb77a3e2e6aae00c4e313dffdc))
* **global-db:** restore measured WAL reclaim to registered checkpoint (Plan 38 §6) ([dc3e3f8](https://github.com/ScriptedAlchemy/tracedecay/commit/dc3e3f83a4fc3aab9976361120cffe61c39f64c7))
* **graph-db:** bind cheap re-seat to an exact dependency digest ([c9cf5d4](https://github.com/ScriptedAlchemy/tracedecay/commit/c9cf5d4459cce8d201852ea944f7cef40569e06b))
* **graph-db:** bind reseat to its generation and single-proof admit ([7669346](https://github.com/ScriptedAlchemy/tracedecay/commit/7669346180f37d4c3a3cceb7c37229a8dab679cf))
* **graph-db:** bound generation retirement pages ([11c6fe4](https://github.com/ScriptedAlchemy/tracedecay/commit/11c6fe48555d0fe1362d5dffa8c25f58b72c5f76))
* **graph-db:** canonicalize operation paths before identity checks ([001a243](https://github.com/ScriptedAlchemy/tracedecay/commit/001a243a35ab8355b03af8f2b20bfc8559de2c21))
* **graph-db:** index vectors introduced by later pages ([6869898](https://github.com/ScriptedAlchemy/tracedecay/commit/68698986cee6e56d3a0250b108a30b2d140be795))
* **graph-db:** keep hydrate digest off the snapshot gate ([5505ec0](https://github.com/ScriptedAlchemy/tracedecay/commit/5505ec0aab79c74cc130de91e7cb4d3b4c06ad1d))
* **graph-db:** scale batch bound and keep budget identity ([2af6170](https://github.com/ScriptedAlchemy/tracedecay/commit/2af6170194a417e3f7d4819cce5fa5e31fb40004))
* **graph-db:** seat live activation after Plan 39 ([0c8373f](https://github.com/ScriptedAlchemy/tracedecay/commit/0c8373fca162d0305e78333bbd92cecb2164a2f0))
* **graph-db:** seat live activation after Plan 39 ([b1741aa](https://github.com/ScriptedAlchemy/tracedecay/commit/b1741aa60b60b30b44c80bcb95370eefcfe60e3d))
* **graph-db:** stage generations in bounded pages ([eb841f7](https://github.com/ScriptedAlchemy/tracedecay/commit/eb841f70d2560526a46f1452a8ac405273ef5b6c))
* **hooks:** index bundled work and workflow skills for cursor ([41e17ee](https://github.com/ScriptedAlchemy/tracedecay/commit/41e17ee2831068d96a6bf584e42ae1fff56859bf))
* **hooks:** keep contended ledger locks typed as busy on windows ([c786587](https://github.com/ScriptedAlchemy/tracedecay/commit/c786587b675b9e9a9f4f0cdc4b211270b5ff7223))
* **hooks:** keep transient spool validation failures typed as io ([0b5c91d](https://github.com/ScriptedAlchemy/tracedecay/commit/0b5c91ddb47710e720f4fd57c8267e012e102cb3))
* **hooks:** refuse non-private existing spool roots ([9cd8732](https://github.com/ScriptedAlchemy/tracedecay/commit/9cd8732a60e4d82a9661aaf37e23b81e8d957bc4))
* **lint:** clear pedantic debt in merged daemon and migrate paths ([6c9a4a2](https://github.com/ScriptedAlchemy/tracedecay/commit/6c9a4a245e5077a493780b3ac88f33787fa21cd5))
* **metadata:** align product release authorities ([92491d8](https://github.com/ScriptedAlchemy/tracedecay/commit/92491d8a89de30e1d3f38fd54486784fb8efe889))
* **observation:** accept bound Codex source revisions ([62ab4c0](https://github.com/ScriptedAlchemy/tracedecay/commit/62ab4c00378cdcdf9a2d0259a3da23a670dbd8f5))
* **observation:** accept semantic anchor replay ([9d6586c](https://github.com/ScriptedAlchemy/tracedecay/commit/9d6586c13feb1233c5ae1a38a50680f8ff14ae72))
* **observation:** converge canonical payload revisions ([fcb452c](https://github.com/ScriptedAlchemy/tracedecay/commit/fcb452c4e1e8039bb3482684bd4d1df1584873dd))
* **observation:** preserve store failure reasons ([328d0b1](https://github.com/ScriptedAlchemy/tracedecay/commit/328d0b1028bcf38ad6119ccfb428ae4dffa06a2e))
* **privacy:** classify ambiguous LCM JSON as quarantine, not receipt construction ([31648fb](https://github.com/ScriptedAlchemy/tracedecay/commit/31648fbb0b82da37431c3e74fadc3d4fdd41d79c))
* **privacy:** close the remaining Codex P1 gaps after [#522](https://github.com/ScriptedAlchemy/tracedecay/issues/522) merged ([fa9c631](https://github.com/ScriptedAlchemy/tracedecay/commit/fa9c6318b3305039893ffa6a13acc8c2e1adb259))
* **privacy:** derive PartialEq for DetectionError ([fd69979](https://github.com/ScriptedAlchemy/tracedecay/commit/fd6997958af9801d7683d52bb8efb2e8cccc7904))
* **privacy:** derive PartialEq for DetectionError ([71d994f](https://github.com/ScriptedAlchemy/tracedecay/commit/71d994fbfc540774c1c7bb452ac909837edee549))
* **privacy:** derive PartialEq for DetectionError ([3cfb111](https://github.com/ScriptedAlchemy/tracedecay/commit/3cfb111c6d0eb5c4da71854de0e30948ff4e4e97))
* **privacy:** preserve sensitive-field quarantine ([4842360](https://github.com/ScriptedAlchemy/tracedecay/commit/4842360b52f1567e27814a02c7ec0527416491d3))
* **privacy:** prove and sweep superseded payload purges ([69354d2](https://github.com/ScriptedAlchemy/tracedecay/commit/69354d2ef640ecab06fdf980df25cf2b670e672d))
* **privacy:** purge detector-flagged superseded payloads on correction ([592de04](https://github.com/ScriptedAlchemy/tracedecay/commit/592de04a7122506c045d24aced9e5bfcfee70b4f))
* **privacy:** quarantine LCM credential-bearing keys instead of Receipt ([dbf8192](https://github.com/ScriptedAlchemy/tracedecay/commit/dbf8192255d3d39c4133e05bbd443bfd9e13fb81))
* **privacy:** quarantine LCM credential-bearing keys instead of Receipt ([5ce4db9](https://github.com/ScriptedAlchemy/tracedecay/commit/5ce4db9755e2c37f074f121c4ed4ab870cba8b1d))
* **privacy:** quarantine LCM credential-bearing keys instead of Receipt ([fdf163d](https://github.com/ScriptedAlchemy/tracedecay/commit/fdf163d578fbdd8889e3d6af9dedfab60421484c))
* **privacy:** route non-JSON key quarantines to CredentialKeyQuarantine ([0103cff](https://github.com/ScriptedAlchemy/tracedecay/commit/0103cffc906385c8a28153d559cd9a4747be02a0))
* **privacy:** route non-JSON key quarantines to CredentialKeyQuarantine ([ae73d9a](https://github.com/ScriptedAlchemy/tracedecay/commit/ae73d9a5a0265929ec7527b021d0e0745dadc2ef))
* **privacy:** route non-JSON key quarantines to CredentialKeyQuarantine ([2d1137a](https://github.com/ScriptedAlchemy/tracedecay/commit/2d1137ae36b15d6661193df23b637df737e42130))
* **privacy:** scan labels and drain retained statuses ([a19bc5a](https://github.com/ScriptedAlchemy/tracedecay/commit/a19bc5a11597bf22a1c3b6aea39d9f7f9a73e651))
* **privacy:** split credential-key quarantine Display ([c79d5c5](https://github.com/ScriptedAlchemy/tracedecay/commit/c79d5c515e1e9790eb9f2ccd3b41ec193b023a33))
* **privacy:** split credential-key quarantine Display ([da5cd31](https://github.com/ScriptedAlchemy/tracedecay/commit/da5cd31a599ad62c0a399176075a6cc7b4697bb4))
* **privacy:** split credential-key quarantine Display ([bce2bf5](https://github.com/ScriptedAlchemy/tracedecay/commit/bce2bf5bb665528f11f0bec82aac0a9b5524c142))
* **projection:** key Claude outputs by row identity ([49acab5](https://github.com/ScriptedAlchemy/tracedecay/commit/49acab5e929a31baa8e76861c2a7c0d3a5b0b407))
* **projection:** retire v4 output ownership atomically ([77efae8](https://github.com/ScriptedAlchemy/tracedecay/commit/77efae8819cb4509614e6b70337cfbf7e059bff6))
* **query:** bound omitted semantic and lexical deadlines ([6e32b35](https://github.com/ScriptedAlchemy/tracedecay/commit/6e32b3516002dd430d849c93deb3f5156d517585))
* **query:** cheaper exact-flat retain + deadline backstop ([8fa116b](https://github.com/ScriptedAlchemy/tracedecay/commit/8fa116bd189d7fc90b2b471f7a4ac052a8035d90))
* **query:** drop Plan 20 framing on budget deadline overrides ([b53812c](https://github.com/ScriptedAlchemy/tracedecay/commit/b53812cbea7d4645e5f3335022e67d4fa62460c2))
* **query:** enforce deadlines across full scans ([ce0b96c](https://github.com/ScriptedAlchemy/tracedecay/commit/ce0b96c72d8a478a00f5e4ef24ccd080090be566))
* **query:** let request budgets override crate deadline fallbacks ([95840eb](https://github.com/ScriptedAlchemy/tracedecay/commit/95840eb072aaa7df3a0258f7fe127ac98b84391e))
* **query:** materialize only retained exact-flat rows ([c1b9e05](https://github.com/ScriptedAlchemy/tracedecay/commit/c1b9e05437e4c0b1ed5e685fb3c643dafae44239))
* **query:** remove unwired lexical budget constructors ([af33753](https://github.com/ScriptedAlchemy/tracedecay/commit/af3375342fbc3280953cbc7e7bb9e28ad78601bd))
* **query:** restore new_admitted_with_budget for daemon mount ([19e3bd7](https://github.com/ScriptedAlchemy/tracedecay/commit/19e3bd7483b56df39c848faa850851b7c74dd816))
* **release:** canonicalize runtime fixture paths ([68b2c34](https://github.com/ScriptedAlchemy/tracedecay/commit/68b2c349be14fd75bb121f10268f1d9cac2d4f91))
* **release:** include Linux runtime notices ([961d555](https://github.com/ScriptedAlchemy/tracedecay/commit/961d5557622d606e40ef192200901f7cd369642a))
* **release:** make beta builders portable ([1116410](https://github.com/ScriptedAlchemy/tracedecay/commit/11164103fd95956315619fa03628ed2187ae1dd0))
* **release:** make harnesses portable on Windows ([f8d457e](https://github.com/ScriptedAlchemy/tracedecay/commit/f8d457e3765964d9320176a612e504a391740f1e))
* **release:** prevent beta lockfile races ([28177d3](https://github.com/ScriptedAlchemy/tracedecay/commit/28177d3303277dfe03631afe5c8223f4b2e6efa5))
* **release:** ship portable Linux inference runtime ([8585bf7](https://github.com/ScriptedAlchemy/tracedecay/commit/8585bf738263370876835d71cea575379d0c7224))
* **release:** use cross-platform Python runtime ([cc22f04](https://github.com/ScriptedAlchemy/tracedecay/commit/cc22f04a5b5a8dbc04239d0d4f1b6c213d5effea))
* **release:** validate beta source before Rust setup ([adecaa0](https://github.com/ScriptedAlchemy/tracedecay/commit/adecaa0eaa37dad94deb2e8063c49530c5c08699))
* **runtime-core:** interrupt WAL folds on cancellation ([ac8356b](https://github.com/ScriptedAlchemy/tracedecay/commit/ac8356bf86332d634dbebeeea331c4408e66a822))
* **runtime-core:** interrupt WAL folds on cancellation ([93ce096](https://github.com/ScriptedAlchemy/tracedecay/commit/93ce0966a58c2472f3bca177aa787dacf607f8ea))
* **runtime-core:** interrupt WAL folds on cancellation ([262f199](https://github.com/ScriptedAlchemy/tracedecay/commit/262f1998e591eac92b943647db03a2a3c00ffb64))
* **runtime-core:** re-interrupt WAL fold until completion ([c068bc1](https://github.com/ScriptedAlchemy/tracedecay/commit/c068bc1949efd67fd2e8ed2ca78adf0755bf84f1))
* **runtime:** isolate repository discovery from blocking pool ([ca8f06c](https://github.com/ScriptedAlchemy/tracedecay/commit/ca8f06cbc2fdb67d989b52b3bd4db68824360e84))
* **runtime:** keep unavailable authorities scoped ([4c4171a](https://github.com/ScriptedAlchemy/tracedecay/commit/4c4171a73ddaf1b3beb08e23e147434147771cf7))
* **runtime:** prefer repository identity authority ([7267953](https://github.com/ScriptedAlchemy/tracedecay/commit/7267953f67dfd979e4df257228b6e9705527fcaf))
* **rusqlite-runtime:** borrow the LazyLock fixture root as &str in placement tests ([4470547](https://github.com/ScriptedAlchemy/tracedecay/commit/44705476e1e80a070bc1ed50f4549970f56d2032))
* **rusqlite-runtime:** drain admitted writes before maintenance ([51f41f7](https://github.com/ScriptedAlchemy/tracedecay/commit/51f41f77584b0f83ec61cb402defa7d350530699))
* **rusqlite-runtime:** drop unused common module from scope set test binary ([ee4bd75](https://github.com/ScriptedAlchemy/tracedecay/commit/ee4bd75b526d95b8a00b9a11481ba5e139340e33))
* **rusqlite-runtime:** expect unused crate-private drain in lib clippy ([f534aa6](https://github.com/ScriptedAlchemy/tracedecay/commit/f534aa6f973a1ab42e4f8880e3518710fa409bbc))
* **rusqlite-runtime:** gate the test-only maintenance drain wrapper ([7480164](https://github.com/ScriptedAlchemy/tracedecay/commit/7480164ebd5dd67892505048b75f20e9dbc597ff))
* **rusqlite-runtime:** keep exclusive drain crate-private ([3305d58](https://github.com/ScriptedAlchemy/tracedecay/commit/3305d5821beabf62d2b8696adce18969c74aca2d))
* **rusqlite-runtime:** keep exclusive drain crate-private ([e69be29](https://github.com/ScriptedAlchemy/tracedecay/commit/e69be294e70b09c8a7b47c94b95fad100c71624e))
* **rusqlite-runtime:** reject blocked inventory before exclusive drain ([9f726be](https://github.com/ScriptedAlchemy/tracedecay/commit/9f726beeae2d6d87f54ed3e52cd0253fb0a32fac))
* **rusqlite-runtime:** reject blocked inventory before exclusive drain ([25f85d6](https://github.com/ScriptedAlchemy/tracedecay/commit/25f85d6a9e8f9862edcc405d37a20fd4f0c2ef76))
* **rusqlite-runtime:** route attempt worktree root through the platform fixture helper ([89a218c](https://github.com/ScriptedAlchemy/tracedecay/commit/89a218c2aa6467bed5b444575040d979cf7b24dd))
* **rusqlite-runtime:** validate maintenance permit before drain ([0862061](https://github.com/ScriptedAlchemy/tracedecay/commit/0862061d74534fdc015912fa03e59e4f718835e4))
* **rusqlite-runtime:** validate maintenance permit before drain ([d0694e7](https://github.com/ScriptedAlchemy/tracedecay/commit/d0694e7dcece02deadd89bb576dca1353a328442))
* **scout:** keep hook cycles live and retryable ([d9e69f7](https://github.com/ScriptedAlchemy/tracedecay/commit/d9e69f72b8ea6d6f6446f2972a3bd57b39407941))
* **search:** report actual fallback lane coverage ([aa85090](https://github.com/ScriptedAlchemy/tracedecay/commit/aa85090c5fd93e24980932de8ae005d39f3832db))
* **semantic:** keep typed cancel/deadline through pre-install warm ([0b870eb](https://github.com/ScriptedAlchemy/tracedecay/commit/0b870eb7fa204ab6867df2c5fa5a95a31303b499))
* **semantic:** preserve candidate warm interruptions ([fd6ada3](https://github.com/ScriptedAlchemy/tracedecay/commit/fd6ada3c30de3ba62d50050e77648b1cca4438a4))
* **semantic:** warm candidate session before installing serving pointer ([32b5c8f](https://github.com/ScriptedAlchemy/tracedecay/commit/32b5c8fd73b758568587ac8e6b10d5dde227862a))
* **sessions:** accept duplicate receipts on read miss ([a553b08](https://github.com/ScriptedAlchemy/tracedecay/commit/a553b0858068db258e62e66c2ba737b7296ca8ff))
* **sessions:** accept duplicate receipts on read miss ([391212c](https://github.com/ScriptedAlchemy/tracedecay/commit/391212c4ee772294fb8a596336bc8131107155a1))
* **sessions:** avoid queued status for duplicate read miss ([2243cdb](https://github.com/ScriptedAlchemy/tracedecay/commit/2243cdbd0755007611b53b0d09d0314c2a931745))
* **sessions:** block cover-past on observation commit failures ([9430898](https://github.com/ScriptedAlchemy/tracedecay/commit/94308981c94a82a3b6f987df276841dfe026592b))
* **sessions:** block cover-past on observation commit failures ([fcc5a76](https://github.com/ScriptedAlchemy/tracedecay/commit/fcc5a76c789d934f9f614d970dbb11308deb9a8a))
* **sessions:** carry admission retry verdict through jsonl ingest wrap ([42cb37d](https://github.com/ScriptedAlchemy/tracedecay/commit/42cb37dd19b04b89fbab3b503ce6f8ccee92fc85))
* **sessions:** keep admission uncertainty typed ([3b26f3c](https://github.com/ScriptedAlchemy/tracedecay/commit/3b26f3c0fde1df9150efab6a6e848d7e35d7e19e))
* **sessions:** keep duplicate projection status unknown ([451855b](https://github.com/ScriptedAlchemy/tracedecay/commit/451855b76418b05d07dca348041ad99666f65e54))
* **skill:** handle current storage status shape ([2d5bf28](https://github.com/ScriptedAlchemy/tracedecay/commit/2d5bf281343044d88611b2003a6211240c5bdbf1))
* **snapshot:** take available_space from private-fs ([f3a883b](https://github.com/ScriptedAlchemy/tracedecay/commit/f3a883b0a0414beb25f2b5ea7ab7a72f1a1902d5))
* **storage:** conflict only on populated exact-root duplicates ([ed81625](https://github.com/ScriptedAlchemy/tracedecay/commit/ed81625fbaad5706f5d78b781a25305222537c28))
* **storage:** surface split identity when candidates name the exact root ([510eeb3](https://github.com/ScriptedAlchemy/tracedecay/commit/510eeb33af00b4dd725107dd0ff13306c212cfb1))
* **storage:** type windows sidecar lock contention as contended ([3f10bba](https://github.com/ScriptedAlchemy/tracedecay/commit/3f10bba3d6ffe23f6d1d948fe91beba260ca197a))
* **test:** align windows file-semantics tests with pin contracts ([21ae992](https://github.com/ScriptedAlchemy/tracedecay/commit/21ae99245e4b52980c8a07b6a8c20f2fa005a797))
* **test:** calibrate exact-sql test budgets for hosted-runner disks ([8a45607](https://github.com/ScriptedAlchemy/tracedecay/commit/8a45607c2a9a31a676bb478c7633aa180f1bc281))
* **test:** platform roots for run-control and leak-adjudication suites ([ffcbd76](https://github.com/ScriptedAlchemy/tracedecay/commit/ffcbd7647fb6d025ddd100156e1a565699055a4d))
* **test:** resolve cargo at runtime for archived metadata probes ([d0d2733](https://github.com/ScriptedAlchemy/tracedecay/commit/d0d27335a07cdd35a8182be62978a434e9ea58fe))
* **test:** restore shared fixture root helper for application tests ([bcde960](https://github.com/ScriptedAlchemy/tracedecay/commit/bcde9604d26aa48d639481cd93db366a96fa70ce))
* **test:** restore shared fixture root helper for application tests ([0ef259f](https://github.com/ScriptedAlchemy/tracedecay/commit/0ef259f92c97c317b182765377954dfab73b8949))
* **test:** restore shared fixture root helper for application tests ([5002c5d](https://github.com/ScriptedAlchemy/tracedecay/commit/5002c5dac5c767ada414fa5426d82e676bb9151d))
* **test:** widen exact-sql test budgets for linux runner cleanup ([c36e06f](https://github.com/ScriptedAlchemy/tracedecay/commit/c36e06f66c12cbd2e6773c91ddabf836e882d98c))
* **usecases:** cheaper LCM drain yield and idle frontier init ([db0099b](https://github.com/ScriptedAlchemy/tracedecay/commit/db0099bbd436a81998a90e18891ad33525c0b68c))
* **usecases:** gate repeat deterministic-refusal drain warns ([c805f24](https://github.com/ScriptedAlchemy/tracedecay/commit/c805f24505f0b104fb3277f6112cc9be71e20024))
* **usecases:** inline one-line drain skip increment ([3b56552](https://github.com/ScriptedAlchemy/tracedecay/commit/3b56552a0e03e2e6c7ae428834a9a0d159fcc866))
* **usecases:** keep draining after durable sanitization skip ([7f84b9b](https://github.com/ScriptedAlchemy/tracedecay/commit/7f84b9b5ae8ed8c47ab566fe6d87280040e03108))
* **usecases:** keep durable refusal diagnostics quiet ([482286a](https://github.com/ScriptedAlchemy/tracedecay/commit/482286a723e69a91f3cdc90f150eb9cd6c4e4d6e))
* **usecases:** make the first idle rollup tick immediate ([cfc5450](https://github.com/ScriptedAlchemy/tracedecay/commit/cfc5450fdb3120d32fad1ecce4b833cb182f63c0))
* **usecases:** type drain yield project-call counter ([ae9824b](https://github.com/ScriptedAlchemy/tracedecay/commit/ae9824bfedc960d07e7180c6149fe1b17c245850))


### Performance Improvements

* **cli:** skip install health checks for inspections ([72d1dbb](https://github.com/ScriptedAlchemy/tracedecay/commit/72d1dbb0aabbad7970c06631fa4a5865be4853b4))
* **code-extraction:** borrow source in hot extract walks ([83086bb](https://github.com/ScriptedAlchemy/tracedecay/commit/83086bb74afb8b22d66d7b2267ec28c083939992))
* **code-extraction:** borrow test callee names ([84951b3](https://github.com/ScriptedAlchemy/tracedecay/commit/84951b3cdbe8d2d119e77e13d5f160a7a92da9c1))
* **code-extraction:** cut no-delimiter signatures at the body child ([95c4e4e](https://github.com/ScriptedAlchemy/tracedecay/commit/95c4e4e764f4b515bbc2a4408674f65f78173856))
* **code-index:** cap default indexing workers ([5366a2c](https://github.com/ScriptedAlchemy/tracedecay/commit/5366a2c63b91871f29e1c2e0384c28765094b019))
* **code-index:** release exact admission staging ([a630505](https://github.com/ScriptedAlchemy/tracedecay/commit/a630505fef57bc35bcf29c55383101b446495390))
* **code-index:** share graph publication manifests ([77da1d7](https://github.com/ScriptedAlchemy/tracedecay/commit/77da1d737242205080331c98679cfebcb69b6d94))
* **code-index:** shrink graph publication and adjacency hydration work ([4aca023](https://github.com/ScriptedAlchemy/tracedecay/commit/4aca023289e12f02a3ccbd608896873c986e9545))
* **daemon:** classify launchd service as interactive ([f5c0da2](https://github.com/ScriptedAlchemy/tracedecay/commit/f5c0da27c6bc6c8858b0724d683067d2830e0e93))
* **dashboard-api:** borrow-keyed hit path for LCM display cache ([074eb21](https://github.com/ScriptedAlchemy/tracedecay/commit/074eb21e4cb0cda2a0b068f3cc7358ee09188e79))
* **dashboard-api:** bound LCM display token cache ([23f6bfc](https://github.com/ScriptedAlchemy/tracedecay/commit/23f6bfc5f7a02b9563f184059d94c4bebd8591c5))
* **dashboard-api:** serve LCM display token counts from shared cache ([620f091](https://github.com/ScriptedAlchemy/tracedecay/commit/620f0912446b388b2034a67bff881c1a47bebbf4))
* **domain:** share sanitized chunk text clones ([ac94b6b](https://github.com/ScriptedAlchemy/tracedecay/commit/ac94b6b150bce939abc8290daa3b988da09b93d0))
* **graph-db:** skip canonical batch rebuild on stored re-seat ([2d6cd0c](https://github.com/ScriptedAlchemy/tracedecay/commit/2d6cd0cec0b6f27cebc4f2421bdbc6ef6e4c4edd))
* **index:** build text serving across worker passes ([4ce6d31](https://github.com/ScriptedAlchemy/tracedecay/commit/4ce6d312f2e4dda6005b5853ab65067911c3da7c))
* **memory:** scan graph source once per settled reconciliation ([44c8a6b](https://github.com/ScriptedAlchemy/tracedecay/commit/44c8a6ba465555e2611e3e115216fbcc9e37c3ec))
* **query:** compact retained lexical scoring state ([09629c0](https://github.com/ScriptedAlchemy/tracedecay/commit/09629c07e8d29f0ae1b1ff26afcbd617c3bb3c4f))
* **query:** resume lexical projection builds ([4817fab](https://github.com/ScriptedAlchemy/tracedecay/commit/4817fab12abcfeb1a419be54ff0e53f4113873d2))
* **runtime-core:** checkpoint WAL snapshot copies in place ([a9c2abd](https://github.com/ScriptedAlchemy/tracedecay/commit/a9c2abd91a47a97e69acc35d4d70f2c6123fe701))
* **runtime-core:** checkpoint WAL snapshot copies in place ([9eb493e](https://github.com/ScriptedAlchemy/tracedecay/commit/9eb493e80e7fa99e10e9db402e2f00ed3d42d1f7))
* **runtime-core:** checkpoint WAL snapshot copies in place ([d2b83e6](https://github.com/ScriptedAlchemy/tracedecay/commit/d2b83e6414f296b76f0325a4c7d7d7eacd2177a1))
* **semantic:** drop eager member byte hashing from lifecycle authority ([fe90ba8](https://github.com/ScriptedAlchemy/tracedecay/commit/fe90ba88bd6f6954023f13e1974fe0b12f4e6844))

## [0.1.0-beta.28](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.27...v0.1.0-beta.28) (2026-08-21)


### Bug Fixes

* **daemon:** honor graph policy for linked worktrees

## [0.1.0-beta.27](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.26...v0.1.0-beta.27) (2026-08-21)


### Bug Fixes

* **projection:** retire v4 output ownership atomically ([77efae8](https://github.com/ScriptedAlchemy/tracedecay/commit/77efae8819cb4509614e6b70337cfbf7e059bff6))

## [0.1.0-beta.26](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.25...v0.1.0-beta.26) (2026-08-21)


### Bug Fixes

* **daemon:** retain session sync tasks through shutdown ([04da6df](https://github.com/ScriptedAlchemy/tracedecay/commit/04da6dfc9015bcba31bba5741792f8b52cfc2c7e))
* **observation:** accept bound Codex source revisions ([62ab4c0](https://github.com/ScriptedAlchemy/tracedecay/commit/62ab4c00378cdcdf9a2d0259a3da23a670dbd8f5))

## [0.1.0-beta.25](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.24...v0.1.0-beta.25) (2026-08-21)


### Performance Improvements

* **domain:** share sanitized chunk text clones ([ac94b6b](https://github.com/ScriptedAlchemy/tracedecay/commit/ac94b6b150bce939abc8290daa3b988da09b93d0))
* **query:** compact retained lexical scoring state ([09629c0](https://github.com/ScriptedAlchemy/tracedecay/commit/09629c07e8d29f0ae1b1ff26afcbd617c3bb3c4f))

## [0.1.0-beta.24](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.23...v0.1.0-beta.24) (2026-08-21)


### Bug Fixes

* **code-index:** refuse graph before retained hydration ([70d091e](https://github.com/ScriptedAlchemy/tracedecay/commit/70d091e7995b92cd79a2b08375099321f1854219))
* **code-index:** update graph policy on remount ([99c50aa](https://github.com/ScriptedAlchemy/tracedecay/commit/99c50aa3c6e9a9d79630fb531eb56f1c6c6a050d))
* **observation:** converge canonical payload revisions ([fcb452c](https://github.com/ScriptedAlchemy/tracedecay/commit/fcb452c4e1e8039bb3482684bd4d1df1584873dd))
* **projection:** key Claude outputs by row identity ([49acab5](https://github.com/ScriptedAlchemy/tracedecay/commit/49acab5e929a31baa8e76861c2a7c0d3a5b0b407))


### Performance Improvements

* **query:** resume lexical projection builds ([4817fab](https://github.com/ScriptedAlchemy/tracedecay/commit/4817fab12abcfeb1a419be54ff0e53f4113873d2))

## [0.1.0-beta.23](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.22...v0.1.0-beta.23) (2026-08-21)


### Bug Fixes

* **config:** converge native graph setting on existing stores ([8c74c58](https://github.com/ScriptedAlchemy/tracedecay/commit/8c74c58dac875f8a03e34be4f69652e676307785))

## [0.1.0-beta.22](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.21...v0.1.0-beta.22) (2026-08-21)


### Bug Fixes

* **claude:** separate row and message identities ([b785e63](https://github.com/ScriptedAlchemy/tracedecay/commit/b785e6327acefbfa8ccda55fbb994b699a0067ec))
* **code-index:** reclaim graph manifest memo ([0dcdd8f](https://github.com/ScriptedAlchemy/tracedecay/commit/0dcdd8f09df6e6bc550594fe9c4e771950473d65))
* **code-index:** serve text when graph budget is refused ([10745a1](https://github.com/ScriptedAlchemy/tracedecay/commit/10745a1e2e109f30b2226c9d24cb1e159ead0cd7))
* **cursor:** stabilize canonical observation context ([cf4868a](https://github.com/ScriptedAlchemy/tracedecay/commit/cf4868acf9cb0f0df38c9702abdd359ec9755195))


### Performance Improvements

* **daemon:** classify launchd service as interactive ([f5c0da2](https://github.com/ScriptedAlchemy/tracedecay/commit/f5c0da27c6bc6c8858b0724d683067d2830e0e93))

## [0.1.0-beta.21](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.20...v0.1.0-beta.21) (2026-08-21)


### Bug Fixes

* **runtime:** prefer repository identity authority ([7267953](https://github.com/ScriptedAlchemy/tracedecay/commit/7267953f67dfd979e4df257228b6e9705527fcaf))

## [0.1.0-beta.20](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.19...v0.1.0-beta.20) (2026-08-21)


### Bug Fixes

* **agent-hosts:** recover versioned registration paths ([bda88ee](https://github.com/ScriptedAlchemy/tracedecay/commit/bda88ee2825694a1eedc256e51e8dfd6c043630f))

## [0.1.0-beta.19](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.18...v0.1.0-beta.19) (2026-08-21)


### Bug Fixes

* **agent-hosts:** preserve multicall interpreter name ([1f50ffd](https://github.com/ScriptedAlchemy/tracedecay/commit/1f50ffdbaf9808d1f951a51f0c18eff1eea9e383))

## [0.1.0-beta.18](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.17...v0.1.0-beta.18) (2026-08-21)


### Bug Fixes

* **daemon:** release session graph leases at shutdown ([b6001dd](https://github.com/ScriptedAlchemy/tracedecay/commit/b6001ddf8d1b7cd10934b54360332aa7710d21cd))
* **daemon:** release session sync stores at shutdown ([d2816ed](https://github.com/ScriptedAlchemy/tracedecay/commit/d2816ed54899a775d0be6e56ce6b1d0e59228a48))
* **daemon:** join project-open background leases ([a8503ee](https://github.com/ScriptedAlchemy/tracedecay/commit/a8503ee761789314afae49178c052e86c35de06e))
* **daemon:** propagate terminal runtime shutdown failures ([b7ed3eb](https://github.com/ScriptedAlchemy/tracedecay/commit/b7ed3eb4290ba2d65b81d1001f145024bb8f0bc2))
* **daemon:** drain terminal graph owner states ([a0310e6](https://github.com/ScriptedAlchemy/tracedecay/commit/a0310e6a8b3a8eadd99a51be812d0ff51c6f748f))
* **daemon:** join active work attempt processes ([f221dc7](https://github.com/ScriptedAlchemy/tracedecay/commit/f221dc776d04053324e1e25671baa83f0fefb327))
* **daemon:** join bounded hook cycle workers ([1f9e429](https://github.com/ScriptedAlchemy/tracedecay/commit/1f9e429bc7ee04cee0ea3a5fdc5f42563afbb9a1))
* **daemon:** latch retirement reaper shutdown wakes ([2567125](https://github.com/ScriptedAlchemy/tracedecay/commit/2567125af033165b3096ee35f69686f660be2ca4))
* **daemon:** reserve scheduler retirement after admission ([41fe56b](https://github.com/ScriptedAlchemy/tracedecay/commit/41fe56b0069e761668de86fb19c500bbb0d08863))
* **daemon:** bound shutdown coordinator receipts ([7c11144](https://github.com/ScriptedAlchemy/tracedecay/commit/7c11144364f8a248fad4f55d2a9598cddff7be8f))
* **daemon:** retain timed-out shutdown runner ownership ([c605133](https://github.com/ScriptedAlchemy/tracedecay/commit/c6051338c4319e4078debe20b9896bcf2b3f2b9a))

## [0.1.0-beta.17](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.16...v0.1.0-beta.17) (2026-08-21)


### Bug Fixes

* **daemon:** join session runtime terminal tasks ([4ba8be4](https://github.com/ScriptedAlchemy/tracedecay/commit/4ba8be4fbf2e76bcd58a3ed6b02690555d8786af))
* **daemon:** release telemetry clients at shutdown ([725e30b](https://github.com/ScriptedAlchemy/tracedecay/commit/725e30b59920a783fe79179fc64455768833f986))

## [0.1.0-beta.16](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.15...v0.1.0-beta.16) (2026-08-21)


### Bug Fixes

* **daemon:** release session graph leases at shutdown ([c3907ff](https://github.com/ScriptedAlchemy/tracedecay/commit/c3907ffbe0f99818842966e4e0ea411c965b915d))
* **daemon:** release session sync stores at shutdown ([3a1b00a](https://github.com/ScriptedAlchemy/tracedecay/commit/3a1b00af3cfd73430366975fba8792bc446b080e))

## [0.1.0-beta.15](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.14...v0.1.0-beta.15) (2026-08-21)


### Bug Fixes

* **cli:** classify read-only startup policy ([bcd937a](https://github.com/ScriptedAlchemy/tracedecay/commit/bcd937a5943cfd72f0b4d0c55127a97eff4c727f))
* **daemon:** preserve exact branch ref identity ([6724e6b](https://github.com/ScriptedAlchemy/tracedecay/commit/6724e6bc2644e230154642f74b4585cd2d58359a))
* **graph-db:** bound generation retirement pages ([11c6fe4](https://github.com/ScriptedAlchemy/tracedecay/commit/11c6fe48555d0fe1362d5dffa8c25f58b72c5f76))
* **graph-db:** index vectors introduced by later pages ([6869898](https://github.com/ScriptedAlchemy/tracedecay/commit/68698986cee6e56d3a0250b108a30b2d140be795))
* **observation:** accept semantic anchor replay ([9d6586c](https://github.com/ScriptedAlchemy/tracedecay/commit/9d6586c13feb1233c5ae1a38a50680f8ff14ae72))

## [0.1.0-beta.14](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.13...v0.1.0-beta.14) (2026-08-21)


### Bug Fixes

* **agent-hosts:** gate receiptless adoption on explicit authority ([6b07061](https://github.com/ScriptedAlchemy/tracedecay/commit/6b07061ad26376437f058a95a8c399b3ab71ec36))
* **agent-hosts:** grant Claude plugin wildcard ([fbf65bf](https://github.com/ScriptedAlchemy/tracedecay/commit/fbf65bf6f2eceb577606979d8dea53f4e21914a7))
* **agent-hosts:** report usable ownership recovery ([9c7ae7d](https://github.com/ScriptedAlchemy/tracedecay/commit/9c7ae7d27823031f41763301fe4faa28c0794dd0))
* **cli:** require confirmed host adoption ([a275cc5](https://github.com/ScriptedAlchemy/tracedecay/commit/a275cc5b0c63ca2b8df5cde4209d6bb4dd1d2481))
* **cli:** skip install checks for nested inspections ([89c1951](https://github.com/ScriptedAlchemy/tracedecay/commit/89c195195146808ff75a33ea7e330c7a2e21be49))
* **code-index:** fail closed on late ceiling install ([63b012c](https://github.com/ScriptedAlchemy/tracedecay/commit/63b012ccaf3496532927edade960f364f57e02de))
* **code-index:** honor daemon worker ceiling ([bf8f4fd](https://github.com/ScriptedAlchemy/tracedecay/commit/bf8f4fd15e9196364a8352a97bf1324761a10e3e))
* **code-index:** serialize worker ceiling setup ([3b371c6](https://github.com/ScriptedAlchemy/tracedecay/commit/3b371c6490dca83728a0d46a8daf9378f1b8a3fc))
* **graph-db:** stage generations in bounded pages ([eb841f7](https://github.com/ScriptedAlchemy/tracedecay/commit/eb841f70d2560526a46f1452a8ac405273ef5b6c))
* **observation:** preserve store failure reasons ([328d0b1](https://github.com/ScriptedAlchemy/tracedecay/commit/328d0b1028bcf38ad6119ccfb428ae4dffa06a2e))

## [0.1.0-beta.13](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.12...v0.1.0-beta.13) (2026-08-21)


### Bug Fixes

* **agent-hosts:** adopt receiptless cataloged deploys on install/update ([8c82f0d](https://github.com/ScriptedAlchemy/tracedecay/commit/8c82f0dd641448f64ac65a566ac72d79cb767316))
* **build:** satisfy Rust 1.97 Clippy policy ([4926a59](https://github.com/ScriptedAlchemy/tracedecay/commit/4926a5939390bb721fe1e6ed5a5318be654d88da))
* **cli:** forward receiptless adoption authority ([e2b173e](https://github.com/ScriptedAlchemy/tracedecay/commit/e2b173e8487721178c94848a9cbbef37e9182a5e))
* **cli:** prevent implicit host repairs ([a762092](https://github.com/ScriptedAlchemy/tracedecay/commit/a76209206e586391998e158ee6f227a0625db451))
* **cli:** update explicit reinstall test imports ([6424fd0](https://github.com/ScriptedAlchemy/tracedecay/commit/6424fd02f36dfd1570f69e5173516bdce78c58b5))
* **daemon:** preserve semantic retries on no-op ([4467464](https://github.com/ScriptedAlchemy/tracedecay/commit/4467464e28d1baca206b42f36fa24f4b4d84527b))
* **daemon:** skip redundant graph activation ([1678492](https://github.com/ScriptedAlchemy/tracedecay/commit/167849236b4515871360c5fecd5d16b17cc981ee))
* **daemon:** skip redundant graph activation ([214781f](https://github.com/ScriptedAlchemy/tracedecay/commit/214781f4e9195f7c1352fbd6f40afe0a020200e0))
* **sessions:** accept duplicate receipts on read miss ([a553b08](https://github.com/ScriptedAlchemy/tracedecay/commit/a553b0858068db258e62e66c2ba737b7296ca8ff))
* **sessions:** accept duplicate receipts on read miss ([391212c](https://github.com/ScriptedAlchemy/tracedecay/commit/391212c4ee772294fb8a596336bc8131107155a1))
* **sessions:** avoid queued status for duplicate read miss ([2243cdb](https://github.com/ScriptedAlchemy/tracedecay/commit/2243cdbd0755007611b53b0d09d0314c2a931745))
* **sessions:** keep duplicate projection status unknown ([451855b](https://github.com/ScriptedAlchemy/tracedecay/commit/451855b76418b05d07dca348041ad99666f65e54))


### Performance Improvements

* **code-index:** release exact admission staging ([a630505](https://github.com/ScriptedAlchemy/tracedecay/commit/a630505fef57bc35bcf29c55383101b446495390))
* **code-index:** share graph publication manifests ([77da1d7](https://github.com/ScriptedAlchemy/tracedecay/commit/77da1d737242205080331c98679cfebcb69b6d94))

## [0.1.0-beta.12](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.11...v0.1.0-beta.12) (2026-08-21)


### Bug Fixes

* **cli:** request the code-index reconcile init reports ([2e066ad](https://github.com/ScriptedAlchemy/tracedecay/commit/2e066ad0f92f934cf14e5ce294158c1a453ad2fe))
* **daemon:** defer slow repository discovery instead of failing open ([39bbac1](https://github.com/ScriptedAlchemy/tracedecay/commit/39bbac1180d5152f30f48f0e9d67ec08117724e3))
* **daemon:** harden first-index recovery ([a4e47ba](https://github.com/ScriptedAlchemy/tracedecay/commit/a4e47ba0964ed4326d17f3a08a7b8dfc6ad85cd3))
* **daemon:** keep the unix socket endpoint within SUN_LEN ([a64e7d6](https://github.com/ScriptedAlchemy/tracedecay/commit/a64e7d6a142d51d88b2582143538a27db0e11b0b))
* **daemon:** stable first-index on empty profiles (discovery deferral, real init reconcile, accept/SUN_LEN hardening) ([5259f5b](https://github.com/ScriptedAlchemy/tracedecay/commit/5259f5b03f24b73c1d27587291ee2f9399adb185))
* **daemon:** survive transient accept errors instead of exiting ([ea0eda9](https://github.com/ScriptedAlchemy/tracedecay/commit/ea0eda9ec7b5795ab8517d0fc5814fe710f285f5))


### Performance Improvements

* **code-index:** cap default indexing workers ([5366a2c](https://github.com/ScriptedAlchemy/tracedecay/commit/5366a2c63b91871f29e1c2e0384c28765094b019))

## [0.1.0-beta.11](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.10...v0.1.0-beta.11) (2026-08-20)


### Bug Fixes

* **daemon:** keep committed-profile unseated retention retryable ([f45e856](https://github.com/ScriptedAlchemy/tracedecay/commit/f45e85641d3096d9d96793e080d9234b60d6c70e))
* **daemon:** quiet vector retention when semantic runtime is unseated ([78509af](https://github.com/ScriptedAlchemy/tracedecay/commit/78509afaccbb21aa99459788a344854ba0c40e0f))
* **runtime:** isolate repository discovery from blocking pool ([ca8f06c](https://github.com/ScriptedAlchemy/tracedecay/commit/ca8f06cbc2fdb67d989b52b3bd4db68824360e84))

## [0.1.0-beta.10](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.9...v0.1.0-beta.10) (2026-08-20)


### Features

* **agent-hosts:** auto-trust Codex managed hooks on install and update ([dbed6d3](https://github.com/ScriptedAlchemy/tracedecay/commit/dbed6d3f18d7a9651d6f790db000088de79d3680))
* **agent-hosts:** auto-trust Codex managed hooks on install and update ([83be52d](https://github.com/ScriptedAlchemy/tracedecay/commit/83be52d382d6f14219063b6a681460de964a8926))


### Bug Fixes

* **agent-hosts:** preserve original mode on host config backups ([f4fbdcb](https://github.com/ScriptedAlchemy/tracedecay/commit/f4fbdcb078b7aea7126490ca813a1da31c14f6dd))
* **agent-hosts:** sync Codex hook trust in activation, prune on removal ([21fdaa0](https://github.com/ScriptedAlchemy/tracedecay/commit/21fdaa073d8a32067a33ad976f9576eacd38c06c))

## [0.1.0-beta.9](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.8...v0.1.0-beta.9) (2026-08-20)


### Bug Fixes

* **release:** prevent beta lockfile races ([28177d3](https://github.com/ScriptedAlchemy/tracedecay/commit/28177d3303277dfe03631afe5c8223f4b2e6efa5))

## [0.1.0-beta.8](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.7...v0.1.0-beta.8) (2026-08-20)


### Bug Fixes

* **release:** make harnesses portable on Windows ([f8d457e](https://github.com/ScriptedAlchemy/tracedecay/commit/f8d457e3765964d9320176a612e504a391740f1e))

## [0.1.0-beta.7](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.6...v0.1.0-beta.7) (2026-08-20)


### Bug Fixes

* **release:** canonicalize runtime fixture paths ([68b2c34](https://github.com/ScriptedAlchemy/tracedecay/commit/68b2c349be14fd75bb121f10268f1d9cac2d4f91))

## [0.1.0-beta.6](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.5...v0.1.0-beta.6) (2026-08-20)


### Bug Fixes

* **release:** include Linux runtime notices ([961d555](https://github.com/ScriptedAlchemy/tracedecay/commit/961d5557622d606e40ef192200901f7cd369642a))

## [0.1.0-beta.5](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.4...v0.1.0-beta.5) (2026-08-20)


### Bug Fixes

* **release:** ship portable Linux inference runtime ([8585bf7](https://github.com/ScriptedAlchemy/tracedecay/commit/8585bf738263370876835d71cea575379d0c7224))

## [0.1.0-beta.4](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.3...v0.1.0-beta.4) (2026-08-20)


### Bug Fixes

* **release:** use cross-platform Python runtime ([cc22f04](https://github.com/ScriptedAlchemy/tracedecay/commit/cc22f04a5b5a8dbc04239d0d4f1b6c213d5effea))
* **release:** validate beta source before Rust setup ([adecaa0](https://github.com/ScriptedAlchemy/tracedecay/commit/adecaa0eaa37dad94deb2e8063c49530c5c08699))

## [0.1.0-beta.3](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.2...v0.1.0-beta.3) (2026-08-20)


### Features

* **automation:** make session-evidence budget backoff window configurable ([1524f03](https://github.com/ScriptedAlchemy/tracedecay/commit/1524f032cca7ac9661a25f721f39acd166563e4d))
* **automation:** typed session-evidence budget backoff for scheduler skips ([dd5b677](https://github.com/ScriptedAlchemy/tracedecay/commit/dd5b6779c770cdd6e25240e35fd3d5c9a1bd8552))
* **private-fs:** mount quota-aware available_space ([747999e](https://github.com/ScriptedAlchemy/tracedecay/commit/747999e9164de414916e0652af4ea1bc874b8f2a))
* **private-fs:** mount quota-aware available_space ([1edb427](https://github.com/ScriptedAlchemy/tracedecay/commit/1edb42763004990b019cd7f57fcccebd517864a4))
* **rusqlite-runtime:** expose exclusive maintenance checkpoint port ([f8de48d](https://github.com/ScriptedAlchemy/tracedecay/commit/f8de48d0c9ef50f2f680c6b3eaf2a1f7b2504152))
* **rusqlite-runtime:** expose exclusive maintenance checkpoint port ([6eb16dd](https://github.com/ScriptedAlchemy/tracedecay/commit/6eb16dd6dd3c3c5808218f34d364dc2f84ba9ae1))
* **rusqlite-runtime:** expose exclusive maintenance checkpoint port ([a13a7f4](https://github.com/ScriptedAlchemy/tracedecay/commit/a13a7f463f47b35eb79e7d08d85b29889aec766b))


### Bug Fixes

* **agent-hosts:** retain profile memory graph port in user harness ([ef23c7c](https://github.com/ScriptedAlchemy/tracedecay/commit/ef23c7c153e0a470e6bee3c1f5f8aa09a68f9ded))
* **application:** bind session evidence budget suppressed label ([320464f](https://github.com/ScriptedAlchemy/tracedecay/commit/320464f2bc5101f2582d37f4c70eb9fdfc1d2697))
* **application:** return typed work-catalog identity failures ([0398bb4](https://github.com/ScriptedAlchemy/tracedecay/commit/0398bb4081db1eb32c90d9587590a31152978179))
* **automation:** dedicate suppressed-tick skip reason and reject zero window ([fbf12e2](https://github.com/ScriptedAlchemy/tracedecay/commit/fbf12e2076739cd62f2d4f198ffc0279279352d4))
* **automation:** distinguish budget backoff skips ([d775002](https://github.com/ScriptedAlchemy/tracedecay/commit/d775002d9f5b935f5a732d7d234b10bfe0a5f15f))
* **automation:** preserve budget backoff ordering ([18f1237](https://github.com/ScriptedAlchemy/tracedecay/commit/18f1237cd6572838f13ebb010743692492c98b14))
* **automation:** share evidence budget labels ([7002612](https://github.com/ScriptedAlchemy/tracedecay/commit/70026120a4967e1db541a486829066cf32e8c2e5))
* **build:** preserve lean feature compilation ([060046f](https://github.com/ScriptedAlchemy/tracedecay/commit/060046fc1bd2e9f554d17d49114cdd239b6300de))
* **cli:** carry the status deadline into daemon tool calls ([dfa752a](https://github.com/ScriptedAlchemy/tracedecay/commit/dfa752a31f79b8edfbf7d975cbacc5797efac401))
* **code-index:** code-index leftover slice for [#421](https://github.com/ScriptedAlchemy/tracedecay/issues/421) — full-scope dispatch, sealed-branch shard key ([fa37dd9](https://github.com/ScriptedAlchemy/tracedecay/commit/fa37dd9779bb31a3d87d8692ba4885436f7f10aa))
* **daemon:** mount budget-aware lexical projection build ([906eea1](https://github.com/ScriptedAlchemy/tracedecay/commit/906eea12c5c299b1f57a311c77c9539e9d72ec20))
* **daemon:** Scout P1 follow-up after [#521](https://github.com/ScriptedAlchemy/tracedecay/issues/521) ([d6a273c](https://github.com/ScriptedAlchemy/tracedecay/commit/d6a273c94317274fe3f66d7926b8c38a1e976bf6))
* **global-db:** restore measured WAL reclaim to registered checkpoint ([28135a5](https://github.com/ScriptedAlchemy/tracedecay/commit/28135a5dc50976eb77a3e2e6aae00c4e313dffdc))
* **global-db:** restore measured WAL reclaim to registered checkpoint (Plan 38 §6) ([dc3e3f8](https://github.com/ScriptedAlchemy/tracedecay/commit/dc3e3f83a4fc3aab9976361120cffe61c39f64c7))
* **graph-db:** bind cheap re-seat to an exact dependency digest ([c9cf5d4](https://github.com/ScriptedAlchemy/tracedecay/commit/c9cf5d4459cce8d201852ea944f7cef40569e06b))
* **graph-db:** bind reseat to its generation and single-proof admit ([7669346](https://github.com/ScriptedAlchemy/tracedecay/commit/7669346180f37d4c3a3cceb7c37229a8dab679cf))
* **graph-db:** keep hydrate digest off the snapshot gate ([5505ec0](https://github.com/ScriptedAlchemy/tracedecay/commit/5505ec0aab79c74cc130de91e7cb4d3b4c06ad1d))
* **graph-db:** seat live activation after Plan 39 ([0c8373f](https://github.com/ScriptedAlchemy/tracedecay/commit/0c8373fca162d0305e78333bbd92cecb2164a2f0))
* **graph-db:** seat live activation after Plan 39 ([b1741aa](https://github.com/ScriptedAlchemy/tracedecay/commit/b1741aa60b60b30b44c80bcb95370eefcfe60e3d))
* **hooks:** index bundled work and workflow skills for cursor ([41e17ee](https://github.com/ScriptedAlchemy/tracedecay/commit/41e17ee2831068d96a6bf584e42ae1fff56859bf))
* **hooks:** keep contended ledger locks typed as busy on windows ([c786587](https://github.com/ScriptedAlchemy/tracedecay/commit/c786587b675b9e9a9f4f0cdc4b211270b5ff7223))
* **hooks:** keep transient spool validation failures typed as io ([0b5c91d](https://github.com/ScriptedAlchemy/tracedecay/commit/0b5c91ddb47710e720f4fd57c8267e012e102cb3))
* **hooks:** refuse non-private existing spool roots ([9cd8732](https://github.com/ScriptedAlchemy/tracedecay/commit/9cd8732a60e4d82a9661aaf37e23b81e8d957bc4))
* **lint:** clear pedantic debt in merged daemon and migrate paths ([6c9a4a2](https://github.com/ScriptedAlchemy/tracedecay/commit/6c9a4a245e5077a493780b3ac88f33787fa21cd5))
* **metadata:** align product release authorities ([92491d8](https://github.com/ScriptedAlchemy/tracedecay/commit/92491d8a89de30e1d3f38fd54486784fb8efe889))
* **privacy:** classify ambiguous LCM JSON as quarantine, not receipt construction ([31648fb](https://github.com/ScriptedAlchemy/tracedecay/commit/31648fbb0b82da37431c3e74fadc3d4fdd41d79c))
* **privacy:** close the remaining Codex P1 gaps after [#522](https://github.com/ScriptedAlchemy/tracedecay/issues/522) merged ([fa9c631](https://github.com/ScriptedAlchemy/tracedecay/commit/fa9c6318b3305039893ffa6a13acc8c2e1adb259))
* **privacy:** derive PartialEq for DetectionError ([fd69979](https://github.com/ScriptedAlchemy/tracedecay/commit/fd6997958af9801d7683d52bb8efb2e8cccc7904))
* **privacy:** derive PartialEq for DetectionError ([71d994f](https://github.com/ScriptedAlchemy/tracedecay/commit/71d994fbfc540774c1c7bb452ac909837edee549))
* **privacy:** derive PartialEq for DetectionError ([3cfb111](https://github.com/ScriptedAlchemy/tracedecay/commit/3cfb111c6d0eb5c4da71854de0e30948ff4e4e97))
* **privacy:** preserve sensitive-field quarantine ([4842360](https://github.com/ScriptedAlchemy/tracedecay/commit/4842360b52f1567e27814a02c7ec0527416491d3))
* **privacy:** prove and sweep superseded payload purges ([69354d2](https://github.com/ScriptedAlchemy/tracedecay/commit/69354d2ef640ecab06fdf980df25cf2b670e672d))
* **privacy:** purge detector-flagged superseded payloads on correction ([592de04](https://github.com/ScriptedAlchemy/tracedecay/commit/592de04a7122506c045d24aced9e5bfcfee70b4f))
* **privacy:** quarantine LCM credential-bearing keys instead of Receipt ([dbf8192](https://github.com/ScriptedAlchemy/tracedecay/commit/dbf8192255d3d39c4133e05bbd443bfd9e13fb81))
* **privacy:** quarantine LCM credential-bearing keys instead of Receipt ([5ce4db9](https://github.com/ScriptedAlchemy/tracedecay/commit/5ce4db9755e2c37f074f121c4ed4ab870cba8b1d))
* **privacy:** quarantine LCM credential-bearing keys instead of Receipt ([fdf163d](https://github.com/ScriptedAlchemy/tracedecay/commit/fdf163d578fbdd8889e3d6af9dedfab60421484c))
* **privacy:** route non-JSON key quarantines to CredentialKeyQuarantine ([0103cff](https://github.com/ScriptedAlchemy/tracedecay/commit/0103cffc906385c8a28153d559cd9a4747be02a0))
* **privacy:** route non-JSON key quarantines to CredentialKeyQuarantine ([ae73d9a](https://github.com/ScriptedAlchemy/tracedecay/commit/ae73d9a5a0265929ec7527b021d0e0745dadc2ef))
* **privacy:** route non-JSON key quarantines to CredentialKeyQuarantine ([2d1137a](https://github.com/ScriptedAlchemy/tracedecay/commit/2d1137ae36b15d6661193df23b637df737e42130))
* **privacy:** split credential-key quarantine Display ([c79d5c5](https://github.com/ScriptedAlchemy/tracedecay/commit/c79d5c515e1e9790eb9f2ccd3b41ec193b023a33))
* **privacy:** split credential-key quarantine Display ([da5cd31](https://github.com/ScriptedAlchemy/tracedecay/commit/da5cd31a599ad62c0a399176075a6cc7b4697bb4))
* **privacy:** split credential-key quarantine Display ([bce2bf5](https://github.com/ScriptedAlchemy/tracedecay/commit/bce2bf5bb665528f11f0bec82aac0a9b5524c142))
* **query:** bound omitted semantic and lexical deadlines ([6e32b35](https://github.com/ScriptedAlchemy/tracedecay/commit/6e32b3516002dd430d849c93deb3f5156d517585))
* **query:** cheaper exact-flat retain + deadline backstop ([8fa116b](https://github.com/ScriptedAlchemy/tracedecay/commit/8fa116bd189d7fc90b2b471f7a4ac052a8035d90))
* **query:** drop Plan 20 framing on budget deadline overrides ([b53812c](https://github.com/ScriptedAlchemy/tracedecay/commit/b53812cbea7d4645e5f3335022e67d4fa62460c2))
* **query:** enforce deadlines across full scans ([ce0b96c](https://github.com/ScriptedAlchemy/tracedecay/commit/ce0b96c72d8a478a00f5e4ef24ccd080090be566))
* **query:** let request budgets override crate deadline fallbacks ([95840eb](https://github.com/ScriptedAlchemy/tracedecay/commit/95840eb072aaa7df3a0258f7fe127ac98b84391e))
* **query:** materialize only retained exact-flat rows ([c1b9e05](https://github.com/ScriptedAlchemy/tracedecay/commit/c1b9e05437e4c0b1ed5e685fb3c643dafae44239))
* **query:** remove unwired lexical budget constructors ([af33753](https://github.com/ScriptedAlchemy/tracedecay/commit/af3375342fbc3280953cbc7e7bb9e28ad78601bd))
* **query:** restore new_admitted_with_budget for daemon mount ([19e3bd7](https://github.com/ScriptedAlchemy/tracedecay/commit/19e3bd7483b56df39c848faa850851b7c74dd816))
* **runtime-core:** interrupt WAL folds on cancellation ([ac8356b](https://github.com/ScriptedAlchemy/tracedecay/commit/ac8356bf86332d634dbebeeea331c4408e66a822))
* **runtime-core:** interrupt WAL folds on cancellation ([93ce096](https://github.com/ScriptedAlchemy/tracedecay/commit/93ce0966a58c2472f3bca177aa787dacf607f8ea))
* **runtime-core:** interrupt WAL folds on cancellation ([262f199](https://github.com/ScriptedAlchemy/tracedecay/commit/262f1998e591eac92b943647db03a2a3c00ffb64))
* **runtime-core:** re-interrupt WAL fold until completion ([c068bc1](https://github.com/ScriptedAlchemy/tracedecay/commit/c068bc1949efd67fd2e8ed2ca78adf0755bf84f1))
* **rusqlite-runtime:** borrow the LazyLock fixture root as &str in placement tests ([4470547](https://github.com/ScriptedAlchemy/tracedecay/commit/44705476e1e80a070bc1ed50f4549970f56d2032))
* **rusqlite-runtime:** drain admitted writes before maintenance ([51f41f7](https://github.com/ScriptedAlchemy/tracedecay/commit/51f41f77584b0f83ec61cb402defa7d350530699))
* **rusqlite-runtime:** drop unused common module from scope set test binary ([ee4bd75](https://github.com/ScriptedAlchemy/tracedecay/commit/ee4bd75b526d95b8a00b9a11481ba5e139340e33))
* **rusqlite-runtime:** expect unused crate-private drain in lib clippy ([f534aa6](https://github.com/ScriptedAlchemy/tracedecay/commit/f534aa6f973a1ab42e4f8880e3518710fa409bbc))
* **rusqlite-runtime:** gate the test-only maintenance drain wrapper ([7480164](https://github.com/ScriptedAlchemy/tracedecay/commit/7480164ebd5dd67892505048b75f20e9dbc597ff))
* **rusqlite-runtime:** keep exclusive drain crate-private ([3305d58](https://github.com/ScriptedAlchemy/tracedecay/commit/3305d5821beabf62d2b8696adce18969c74aca2d))
* **rusqlite-runtime:** keep exclusive drain crate-private ([e69be29](https://github.com/ScriptedAlchemy/tracedecay/commit/e69be294e70b09c8a7b47c94b95fad100c71624e))
* **rusqlite-runtime:** reject blocked inventory before exclusive drain ([9f726be](https://github.com/ScriptedAlchemy/tracedecay/commit/9f726beeae2d6d87f54ed3e52cd0253fb0a32fac))
* **rusqlite-runtime:** reject blocked inventory before exclusive drain ([25f85d6](https://github.com/ScriptedAlchemy/tracedecay/commit/25f85d6a9e8f9862edcc405d37a20fd4f0c2ef76))
* **rusqlite-runtime:** route attempt worktree root through the platform fixture helper ([89a218c](https://github.com/ScriptedAlchemy/tracedecay/commit/89a218c2aa6467bed5b444575040d979cf7b24dd))
* **rusqlite-runtime:** validate maintenance permit before drain ([0862061](https://github.com/ScriptedAlchemy/tracedecay/commit/0862061d74534fdc015912fa03e59e4f718835e4))
* **rusqlite-runtime:** validate maintenance permit before drain ([d0694e7](https://github.com/ScriptedAlchemy/tracedecay/commit/d0694e7dcece02deadd89bb576dca1353a328442))
* **semantic:** keep typed cancel/deadline through pre-install warm ([0b870eb](https://github.com/ScriptedAlchemy/tracedecay/commit/0b870eb7fa204ab6867df2c5fa5a95a31303b499))
* **semantic:** preserve candidate warm interruptions ([fd6ada3](https://github.com/ScriptedAlchemy/tracedecay/commit/fd6ada3c30de3ba62d50050e77648b1cca4438a4))
* **semantic:** warm candidate session before installing serving pointer ([32b5c8f](https://github.com/ScriptedAlchemy/tracedecay/commit/32b5c8fd73b758568587ac8e6b10d5dde227862a))
* **sessions:** block cover-past on observation commit failures ([9430898](https://github.com/ScriptedAlchemy/tracedecay/commit/94308981c94a82a3b6f987df276841dfe026592b))
* **sessions:** block cover-past on observation commit failures ([fcc5a76](https://github.com/ScriptedAlchemy/tracedecay/commit/fcc5a76c789d934f9f614d970dbb11308deb9a8a))
* **sessions:** keep admission uncertainty typed ([3b26f3c](https://github.com/ScriptedAlchemy/tracedecay/commit/3b26f3c0fde1df9150efab6a6e848d7e35d7e19e))
* **snapshot:** take available_space from private-fs ([f3a883b](https://github.com/ScriptedAlchemy/tracedecay/commit/f3a883b0a0414beb25f2b5ea7ab7a72f1a1902d5))
* **storage:** conflict only on populated exact-root duplicates ([ed81625](https://github.com/ScriptedAlchemy/tracedecay/commit/ed81625fbaad5706f5d78b781a25305222537c28))
* **storage:** surface split identity when candidates name the exact root ([510eeb3](https://github.com/ScriptedAlchemy/tracedecay/commit/510eeb33af00b4dd725107dd0ff13306c212cfb1))
* **storage:** type windows sidecar lock contention as contended ([3f10bba](https://github.com/ScriptedAlchemy/tracedecay/commit/3f10bba3d6ffe23f6d1d948fe91beba260ca197a))
* **test:** align windows file-semantics tests with pin contracts ([21ae992](https://github.com/ScriptedAlchemy/tracedecay/commit/21ae99245e4b52980c8a07b6a8c20f2fa005a797))
* **test:** calibrate exact-sql test budgets for hosted-runner disks ([8a45607](https://github.com/ScriptedAlchemy/tracedecay/commit/8a45607c2a9a31a676bb478c7633aa180f1bc281))
* **test:** platform roots for run-control and leak-adjudication suites ([ffcbd76](https://github.com/ScriptedAlchemy/tracedecay/commit/ffcbd7647fb6d025ddd100156e1a565699055a4d))
* **test:** resolve cargo at runtime for archived metadata probes ([d0d2733](https://github.com/ScriptedAlchemy/tracedecay/commit/d0d27335a07cdd35a8182be62978a434e9ea58fe))
* **test:** restore shared fixture root helper for application tests ([bcde960](https://github.com/ScriptedAlchemy/tracedecay/commit/bcde9604d26aa48d639481cd93db366a96fa70ce))
* **test:** restore shared fixture root helper for application tests ([0ef259f](https://github.com/ScriptedAlchemy/tracedecay/commit/0ef259f92c97c317b182765377954dfab73b8949))
* **test:** restore shared fixture root helper for application tests ([5002c5d](https://github.com/ScriptedAlchemy/tracedecay/commit/5002c5dac5c767ada414fa5426d82e676bb9151d))
* **test:** widen exact-sql test budgets for linux runner cleanup ([c36e06f](https://github.com/ScriptedAlchemy/tracedecay/commit/c36e06f66c12cbd2e6773c91ddabf836e882d98c))
* **usecases:** cheaper LCM drain yield and idle frontier init ([db0099b](https://github.com/ScriptedAlchemy/tracedecay/commit/db0099bbd436a81998a90e18891ad33525c0b68c))
* **usecases:** gate repeat deterministic-refusal drain warns ([c805f24](https://github.com/ScriptedAlchemy/tracedecay/commit/c805f24505f0b104fb3277f6112cc9be71e20024))
* **usecases:** inline one-line drain skip increment ([3b56552](https://github.com/ScriptedAlchemy/tracedecay/commit/3b56552a0e03e2e6c7ae428834a9a0d159fcc866))
* **usecases:** keep draining after durable sanitization skip ([7f84b9b](https://github.com/ScriptedAlchemy/tracedecay/commit/7f84b9b5ae8ed8c47ab566fe6d87280040e03108))
* **usecases:** keep durable refusal diagnostics quiet ([482286a](https://github.com/ScriptedAlchemy/tracedecay/commit/482286a723e69a91f3cdc90f150eb9cd6c4e4d6e))
* **usecases:** make the first idle rollup tick immediate ([cfc5450](https://github.com/ScriptedAlchemy/tracedecay/commit/cfc5450fdb3120d32fad1ecce4b833cb182f63c0))
* **usecases:** type drain yield project-call counter ([ae9824b](https://github.com/ScriptedAlchemy/tracedecay/commit/ae9824bfedc960d07e7180c6149fe1b17c245850))


### Performance Improvements

* **code-extraction:** borrow source in hot extract walks ([83086bb](https://github.com/ScriptedAlchemy/tracedecay/commit/83086bb74afb8b22d66d7b2267ec28c083939992))
* **code-extraction:** borrow test callee names ([84951b3](https://github.com/ScriptedAlchemy/tracedecay/commit/84951b3cdbe8d2d119e77e13d5f160a7a92da9c1))
* **code-extraction:** cut no-delimiter signatures at the body child ([95c4e4e](https://github.com/ScriptedAlchemy/tracedecay/commit/95c4e4e764f4b515bbc2a4408674f65f78173856))
* **code-index:** shrink graph publication and adjacency hydration work ([4aca023](https://github.com/ScriptedAlchemy/tracedecay/commit/4aca023289e12f02a3ccbd608896873c986e9545))
* **dashboard-api:** borrow-keyed hit path for LCM display cache ([074eb21](https://github.com/ScriptedAlchemy/tracedecay/commit/074eb21e4cb0cda2a0b068f3cc7358ee09188e79))
* **dashboard-api:** bound LCM display token cache ([23f6bfc](https://github.com/ScriptedAlchemy/tracedecay/commit/23f6bfc5f7a02b9563f184059d94c4bebd8591c5))
* **dashboard-api:** serve LCM display token counts from shared cache ([620f091](https://github.com/ScriptedAlchemy/tracedecay/commit/620f0912446b388b2034a67bff881c1a47bebbf4))
* **graph-db:** skip canonical batch rebuild on stored re-seat ([2d6cd0c](https://github.com/ScriptedAlchemy/tracedecay/commit/2d6cd0cec0b6f27cebc4f2421bdbc6ef6e4c4edd))
* **memory:** scan graph source once per settled reconciliation ([44c8a6b](https://github.com/ScriptedAlchemy/tracedecay/commit/44c8a6ba465555e2611e3e115216fbcc9e37c3ec))
* **runtime-core:** checkpoint WAL snapshot copies in place ([a9c2abd](https://github.com/ScriptedAlchemy/tracedecay/commit/a9c2abd91a47a97e69acc35d4d70f2c6123fe701))
* **runtime-core:** checkpoint WAL snapshot copies in place ([9eb493e](https://github.com/ScriptedAlchemy/tracedecay/commit/9eb493e80e7fa99e10e9db402e2f00ed3d42d1f7))
* **runtime-core:** checkpoint WAL snapshot copies in place ([d2b83e6](https://github.com/ScriptedAlchemy/tracedecay/commit/d2b83e6414f296b76f0325a4c7d7d7eacd2177a1))
* **semantic:** drop eager member byte hashing from lifecycle authority ([fe90ba8](https://github.com/ScriptedAlchemy/tracedecay/commit/fe90ba88bd6f6954023f13e1974fe0b12f4e6844))

## [0.1.0-beta.2](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta.1...v0.1.0-beta.2) (2026-08-19)


### Features

* mount privacy remediation, multi-root dashboard scope, native-integration notifications ([70182de](https://github.com/ScriptedAlchemy/tracedecay/commit/70182de3eae04a3cf81b03d6b2274de8fc0536cb))
* **privacy:** rescan at-rest LCM store bytes under current detector ([1c58d39](https://github.com/ScriptedAlchemy/tracedecay/commit/1c58d39caf5311ff30bb121cdc1e71ec68764632))
* **privacy:** rescan at-rest LCM store bytes under the current detector ([327011a](https://github.com/ScriptedAlchemy/tracedecay/commit/327011a7460f2ec220b50b96ead256e85f87c097))
* **scout:** remount the Context Scout producer path and Plan 20 flag ([ab6099e](https://github.com/ScriptedAlchemy/tracedecay/commit/ab6099e45651a249d7699cb2300cd8764a0cea66))


### Bug Fixes

* **daemon:** harden project privacy and status routing ([f1655c8](https://github.com/ScriptedAlchemy/tracedecay/commit/f1655c8ae3efc44c229f7ee7eab760a1aec57558))
* **daemon:** join reconciliation workers before closing retained graphs ([59b3760](https://github.com/ScriptedAlchemy/tracedecay/commit/59b37600047d2abc5909275986c1f00eb85055c6))
* **graph-db:** scale batch bound and keep budget identity ([2af6170](https://github.com/ScriptedAlchemy/tracedecay/commit/2af6170194a417e3f7d4819cce5fa5e31fb40004))
* **privacy:** scan labels and drain retained statuses ([a19bc5a](https://github.com/ScriptedAlchemy/tracedecay/commit/a19bc5a11597bf22a1c3b6aea39d9f7f9a73e651))
* **scout:** keep hook cycles live and retryable ([d9e69f7](https://github.com/ScriptedAlchemy/tracedecay/commit/d9e69f72b8ea6d6f6446f2972a3bd57b39407941))
* **sessions:** carry admission retry verdict through jsonl ingest wrap ([42cb37d](https://github.com/ScriptedAlchemy/tracedecay/commit/42cb37dd19b04b89fbab3b503ce6f8ccee92fc85))

## [0.1.0-beta.1](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.1.0-beta...v0.1.0-beta.1) (2026-08-19)


### Bug Fixes

* **graph-db:** canonicalize operation paths before identity checks ([001a243](https://github.com/ScriptedAlchemy/tracedecay/commit/001a243a35ab8355b03af8f2b20bfc8559de2c21))

## [0.1.0-beta](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.74...v0.1.0-beta) (2026-08-19)


### Features

* **observability:** mount adoption census and outcome funnel ([5faa6fe](https://github.com/ScriptedAlchemy/tracedecay/commit/5faa6fe270e2ced270fbc50e275fc22a57c7c0fd))
* **observability:** mount github stack capability and drift receipts ([04b1dad](https://github.com/ScriptedAlchemy/tracedecay/commit/04b1dad4f912b6bec603731796c31ed592e300c0))
* **observability:** mount index lifecycle and retrieval-pipeline lanes ([f176b81](https://github.com/ScriptedAlchemy/tracedecay/commit/f176b819f0d3b7416f79b56c0b196b5dff639186))
* **observability:** mount no-progress terminals for wall-exhausted work ([f6d5c55](https://github.com/ScriptedAlchemy/tracedecay/commit/f6d5c55dda76fb93dd964c426395888572bb319b))
* **observability:** mount unmounted emitters and generation-bound joins ([ac471d7](https://github.com/ScriptedAlchemy/tracedecay/commit/ac471d7eb4014e2d7ffdb121805f200bd21f914f))
* **observability:** mount work-conflict prediction and outcome receipts ([fc7e248](https://github.com/ScriptedAlchemy/tracedecay/commit/fc7e248fd19236fbb9ab502ca2666bf33a209167))
* **workflow:** mount the remaining Work/workflow product surface ([3cfccec](https://github.com/ScriptedAlchemy/tracedecay/commit/3cfccec036a6e359918120133fccd8fad0673eb2))


### Bug Fixes

* **cli:** compose the caller's own spool evidence for remote query ([d1973f4](https://github.com/ScriptedAlchemy/tracedecay/commit/d1973f4c2852a2fb6636507959461491ae921561))
* **daemon-client:** type post-cancel transport failures as indeterminate ([f0dacc1](https://github.com/ScriptedAlchemy/tracedecay/commit/f0dacc1816055c024b66cb8f83e75398e2d9df1c))
* **daemon:** report an uncomposable catalog as unavailable on activate ([997d897](https://github.com/ScriptedAlchemy/tracedecay/commit/997d897342643aad0e780f68638f89e26c07304c))
* **dashboard-api:** serve selected-project workflow reads ([e4c8b51](https://github.com/ScriptedAlchemy/tracedecay/commit/e4c8b516909719e7a68c6c7732ba03806f1e215d))
* **dashboard:** refuse workflow lifecycle writes in read-only scope ([e27d3c6](https://github.com/ScriptedAlchemy/tracedecay/commit/e27d3c6701790a80d3d8930c6014e042bf34e308))
* **dashboard:** require a readable cell before an integration headline ([ed852d4](https://github.com/ScriptedAlchemy/tracedecay/commit/ed852d4c5acc5c49a1748ff1d23d494fff9c242f))
* **dashboard:** reset workflow lifecycle controls per definition ([fe230de](https://github.com/ScriptedAlchemy/tracedecay/commit/fe230dea09571b919662d409fe11b5cf3ad94d3e))
* **db:** restore the consolidation authority restriction lost in merge ([812fff4](https://github.com/ScriptedAlchemy/tracedecay/commit/812fff47fd44fbebf654b124792fdfcf2742a072))
* **graph-db:** size generation capacity for real repository scale ([273b1ef](https://github.com/ScriptedAlchemy/tracedecay/commit/273b1ef33861ef777c909f34dc0568eaab62840f))
* **hooks:** keep contended ledger locks typed as busy on windows ([2ef0488](https://github.com/ScriptedAlchemy/tracedecay/commit/2ef0488044c096ef0e270d3b36abd8437e91b6a8))
* **hooks:** keep transient spool validation failures typed as io ([02ccf17](https://github.com/ScriptedAlchemy/tracedecay/commit/02ccf178c95e279299593be20c9e8c986e0ea7f2))
* **hooks:** refuse non-private existing spool roots ([aaed3b2](https://github.com/ScriptedAlchemy/tracedecay/commit/aaed3b2e87f593e9d738155368b2e01ee0c67b77))
* **lint:** clear pedantic debt in merged daemon and migrate paths ([e814221](https://github.com/ScriptedAlchemy/tracedecay/commit/e814221e1e185489e8da873356424531629e5f29))
* **release:** align server.json with release 0.0.74 ([ad257ba](https://github.com/ScriptedAlchemy/tracedecay/commit/ad257babf7694688ad252595d2b13ddd1d773cec))
* **sdk:** never proxy loopback remote daemon requests ([bd1e080](https://github.com/ScriptedAlchemy/tracedecay/commit/bd1e0801d7e06b9185a30d3205bd5841f79ca9a5))
* **sdk:** never route the loopback remote target through a proxy ([3c0f269](https://github.com/ScriptedAlchemy/tracedecay/commit/3c0f269d4dc9f0f67e1a54024bf53f7dfbfed62f))
* **storage:** conflict only on populated exact-root duplicates ([b3f5c11](https://github.com/ScriptedAlchemy/tracedecay/commit/b3f5c11ef65a700e543f36d18ddf61035d79d92d))
* **storage:** surface split identity when candidates name the exact root ([f7040ae](https://github.com/ScriptedAlchemy/tracedecay/commit/f7040aecc4b4f19aae93167ce95a6d9a72892e15))
* **storage:** type windows sidecar lock contention as contended ([159caae](https://github.com/ScriptedAlchemy/tracedecay/commit/159caaee501757261ca2b39e271a1b1302a70a4c))
* **test:** align windows file-semantics tests with pin contracts ([2bf81d9](https://github.com/ScriptedAlchemy/tracedecay/commit/2bf81d9ca886d9fc5a8d66f42c7af68367a157f3))
* **test:** calibrate exact-sql test budgets for hosted-runner disks ([34ccacc](https://github.com/ScriptedAlchemy/tracedecay/commit/34ccacce07bd26cf40f498369db6f85be9fdd77a))
* **test:** platform roots for run-control and leak-adjudication suites ([12352b5](https://github.com/ScriptedAlchemy/tracedecay/commit/12352b59d0e5a00722c040fd1346b0f5ce22519f))
* **test:** widen exact-sql test budgets for linux runner cleanup ([a91754d](https://github.com/ScriptedAlchemy/tracedecay/commit/a91754d3621e2fa1aaa0c1720c843682e8db5285))
* **windows:** gate unix-only tests and expose portable inventory seams ([9b8599f](https://github.com/ScriptedAlchemy/tracedecay/commit/9b8599f73282536885af6bfee57f461c09b005b5))

## [0.0.74](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.73...v0.0.74) (2026-08-19)


### Bug Fixes

* **daemon:** abort watchers before joins ([fa93535](https://github.com/ScriptedAlchemy/tracedecay/commit/fa935352e75c9cdb9b22ea3f513ac247c002eaae))
* **daemon:** bound maintenance shutdown ([caba6eb](https://github.com/ScriptedAlchemy/tracedecay/commit/caba6eba7ae2a41bcd9c8827230a39db101331b0))
* **db:** restrict consolidation authority ([a556506](https://github.com/ScriptedAlchemy/tracedecay/commit/a556506d2eaf460fc62fb6f74603c17dc043e5fc))
* **migrate:** authorize staged databases ([850414b](https://github.com/ScriptedAlchemy/tracedecay/commit/850414bb1a91a3ee3ea89470a14de455332a9054))
* **storage:** fail closed on unreadable inventory ([6e92ed4](https://github.com/ScriptedAlchemy/tracedecay/commit/6e92ed4f3d5501700a666b3b103119413b70d6d1))
* **storage:** harden project store authority and resolution ([3050018](https://github.com/ScriptedAlchemy/tracedecay/commit/30500186caa9375d3daa145aee0f88803e51792f))
* **storage:** harden recovery validation ([71b19fc](https://github.com/ScriptedAlchemy/tracedecay/commit/71b19fc1f9ed03e8a5be7507d7d241b3dffd18aa))
* **storage:** honor aliases with repository markers ([91944bc](https://github.com/ScriptedAlchemy/tracedecay/commit/91944bc6ce0828e9556411de45be135249a2a677))
* **storage:** isolate branch recovery markers ([82876ea](https://github.com/ScriptedAlchemy/tracedecay/commit/82876ea591988eb0e3d242f1e3d727a18991d81d))
* **storage:** preserve exact-root precedence ([df96e9b](https://github.com/ScriptedAlchemy/tracedecay/commit/df96e9b5c2ba3aba162ab47c91777567d17ae181))
* **storage:** reject empty recovery stores ([043a047](https://github.com/ScriptedAlchemy/tracedecay/commit/043a047e03b87cdeb0bf77241a852ec2bf0f80d7))
* **storage:** trust exact worktree registry aliases ([a79f70c](https://github.com/ScriptedAlchemy/tracedecay/commit/a79f70cc3bb4b307ff17444a3adafaae8b52f670))
* **storage:** validate auxiliary health first ([a818fcd](https://github.com/ScriptedAlchemy/tracedecay/commit/a818fcdcbc8f90e00fbf95437f43dd05e41001e3))


### Performance Improvements

* **mcp:** reuse active selected project ([726ce54](https://github.com/ScriptedAlchemy/tracedecay/commit/726ce54d4a2578efa8bf4774e898f4364d25bcdf))
* **session:** bound correlation presence checks ([25b67d1](https://github.com/ScriptedAlchemy/tracedecay/commit/25b67d1f526f116398701d47753e877e85cd4c56))
* **storage:** avoid duplicate recovery scans ([2467ff9](https://github.com/ScriptedAlchemy/tracedecay/commit/2467ff9dc5609916a28304384f0b1341639e3050))
* **storage:** avoid full counts on exact roots ([ab7be60](https://github.com/ScriptedAlchemy/tracedecay/commit/ab7be60b9164afe6b2100b51bdaabe94c1380b09))
* **storage:** bound conflict inventory ([a299410](https://github.com/ScriptedAlchemy/tracedecay/commit/a2994101fb83565eae98151308fabb68682ce2e3))
* **storage:** check the serving branch graph ([ac0b0c9](https://github.com/ScriptedAlchemy/tracedecay/commit/ac0b0c91b45117069629b29adf1fb5300dfbef4f))
* **storage:** trust healthy exact root among duplicates ([133ca5c](https://github.com/ScriptedAlchemy/tracedecay/commit/133ca5c48bac9ecfb054f9ea59e6435feeca0e94))

## [0.0.73](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.72...v0.0.73) (2026-08-04)


### Bug Fixes

* **cli:** retrieve truncated daemon JSON ([#503](https://github.com/ScriptedAlchemy/tracedecay/issues/503)) ([bfc2825](https://github.com/ScriptedAlchemy/tracedecay/commit/bfc2825c87dd4903538f2d2bede1f86bb4e1590f))

## [0.0.72](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.71...v0.0.72) (2026-08-04)


### Bug Fixes

* **update:** quiesce daemon before lifecycle lock ([#501](https://github.com/ScriptedAlchemy/tracedecay/issues/501)) ([cfa22ae](https://github.com/ScriptedAlchemy/tracedecay/commit/cfa22aeb7b1fb7bbb5bdb3625f8d31df1f87ab23))

## [0.0.71](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.70...v0.0.71) (2026-08-04)


### Bug Fixes

* **update:** reuse lifecycle lease for Hermes refresh ([#499](https://github.com/ScriptedAlchemy/tracedecay/issues/499)) ([19ff99a](https://github.com/ScriptedAlchemy/tracedecay/commit/19ff99a53ab20173027041967bb477b05a819a7e))

## [0.0.70](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.69...v0.0.70) (2026-08-04)


### Bug Fixes

* **release:** tolerate missing release PR output ([b39adb8](https://github.com/ScriptedAlchemy/tracedecay/commit/b39adb812e7efaed33db9ff4f2cd22c5ecf849b0))

## [0.0.69](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.68...v0.0.69) (2026-08-04)


### Bug Fixes

* address unresolved review and release issues ([#492](https://github.com/ScriptedAlchemy/tracedecay/issues/492)) ([f4510b4](https://github.com/ScriptedAlchemy/tracedecay/commit/f4510b453ff9b8d6a789e6ef8edfb0d6fbc3670d))
* **daemon:** close startup readiness races ([#494](https://github.com/ScriptedAlchemy/tracedecay/issues/494)) ([b9b8c25](https://github.com/ScriptedAlchemy/tracedecay/commit/b9b8c252284652d7a1844bbcc29b48c4e1e080c1))
* **release:** keep root lockfile version aligned ([8a2018e](https://github.com/ScriptedAlchemy/tracedecay/commit/8a2018e8743de8a2852a169ca8a11db7020d9792))
* **release:** switch stable distribution to GitHub assets ([6217363](https://github.com/ScriptedAlchemy/tracedecay/commit/6217363f0bacf0b033f5ab456f645615f6e54484))
* **release:** update root lockfile deterministically ([#493](https://github.com/ScriptedAlchemy/tracedecay/issues/493)) ([3ee78a8](https://github.com/ScriptedAlchemy/tracedecay/commit/3ee78a8c9c39874548c1bb9220a3c45958e71641))
* **release:** use package-neutral GitHub tags ([#495](https://github.com/ScriptedAlchemy/tracedecay/issues/495)) ([bf78342](https://github.com/ScriptedAlchemy/tracedecay/commit/bf7834235968f952d0eb4dc897494eaec19fbe08))

## [Unreleased]

### Removed

- *(cli)* for the explicit 0.1.0 breaking release, remove the shipped
  `claude-install`, `update-plugins`, and `claude-uninstall` host lifecycle
  aliases. Use `install`, `update-plugin`, and `uninstall`; the old spellings
  now fail as invalid subcommands.
- *(storage)* retire the `tracedecay-migrate` crate and the v0.0.67
  released-shape migration exception. Operators on a v0.0.67-era store shape
  now get a typed `ResetRequired` instead of an in-place upgrade path; this
  is a deliberate removal (fresh-store/final-schema doctrine), not a
  regression.

## [0.0.68](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.67...v0.0.68) - 2026-08-03

### Fixed

- *(cargo)* isolate nested worktrees without dropping packaged dashboard assets
- *(grep)* scope generated path glob traversal
- *(grep)* honor generated directory path globs
- *(grep)* bound filesystem scans
- *(sessions)* preserve and retry uncertain worktree membership
- *(sessions)* defer uncertain session routing
- *(storage)* bound legacy worktree probing
- *(worktrees)* avoid blocking Git discovery

### Performance

- *(sessions)* cache worktree metadata resolution

## [0.0.67](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.66...v0.0.67) - 2026-07-27

### Fixed

- *(daemon)* bound project open only when a writer is busy
- *(storage)* keep store inventory off the resolve path
- *(storage)* sync preserved recovery set on Windows
- auto-repair corrupt branch stores
- restore daemon catalog build
- *(daemon)* raise managed service file limit
- address follow-up storage and daemon findings
- address daemon and storage review findings
- *(daemon)* cover full shutdown window
- *(daemon)* wait for lifecycle lease release
- *(daemon)* unblock managed service restart
- *(daemon)* coordinate scheduler discovery
- *(daemon)* unblock uncached worktree tools
- *(daemon)* drain portable warmups safely
- *(mcp)* preserve portable bootstrap parity
- *(mcp)* unblock fresh daemon startup
- *(daemon)* keep cached MCP servers responsive
- *(storage)* prefer healthy exact worktree shard
- *(storage)* prefer exact worktree manifest root
- *(doctor)* use compact daemon health snapshot
- *(tests)* harden Windows daemon fixtures
- *(storage)* harden cross-platform daemon authority
- *(daemon)* release legacy sync ownership after writes
- *(daemon)* reuse global database authority
- *(storage)* stabilize daemon-owned database access
- *(tests)* isolate database ownership fixtures
- *(storage)* clear integrity clippy blockers
- *(storage)* repair integrity follow-up build
- *(storage)* close sole-daemon integrity gaps
- *(storage)* finish sole-daemon ownership integration
- *(storage)* enforce sole-daemon database ownership

### Other

- Merge remote-tracking branch 'origin/master' into pr-478-fmt
- *(daemon)* name the MCP bootstrap outcome enum
- apply rustfmt to daemon and storage changes
- *(storage)* normalize Windows authority paths
- *(daemon)* share initialize registry authority

## [0.0.66](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.65...v0.0.66) - 2026-07-13

### Fixed

- *(storage)* preflight dirty graph recovery read-only

## [0.0.65](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.64...v0.0.65) - 2026-07-13

### Fixed

- *(migrate)* report invalid reconstruction metadata
- *(ci)* satisfy clippy let-else lint
- *(migrate)* align branch metadata validation

### Other

- Merge branch 'master' into fix/consolidate-legacy-single-db

## [0.0.64](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.63...v0.0.64) - 2026-07-12

### Fixed

- *(storage)* close snapshots before scratch cleanup
- *(storage)* normalize Windows consolidation state
- *(hermes)* trust backend replay shrink estimates
- *(hermes)* preserve atomic LCM tool transactions
- *(storage)* preserve dirty markers in backups
- *(memory)* preserve project store on branch databases
- *(memory)* reuse daemon database handle
- *(storage)* exclude runtime locks from backups
- *(storage)* report locked migration copy path
- *(storage)* avoid Windows snapshot copy locks

## [0.0.63](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.62...v0.0.63) - 2026-07-12

### Fixed

- *(upgrade)* show safe shutdown progress

## [0.0.62](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.61...v0.0.62) - 2026-07-12

### Fixed

- *(automation)* protect managed skill ownership

## [0.0.61](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.60...v0.0.61) - 2026-07-12

### Fixed

- *(automation)* isolate managed skill test exports

## [0.0.60](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.59...v0.0.60) - 2026-07-12

### Fixed

- *(hooks)* preserve compact agent discovery
- *(daemon)* bound runtime teardown
- *(daemon)* bound background task shutdown
- *(hooks)* keep Codex steering within compact budget
- *(agents)* document JSON tool fallback arguments
- *(hermes)* preserve replay identity during compression
- *(memory)* defer vacuum until exclusive maintenance

## [0.0.59](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.58...v0.0.59) - 2026-07-12

### Added

- *(dev)* add stable dogfood install workflow

### Fixed

- *(dev)* launch dogfood without nested cargo
- *(hermes)* correlate turns across project scopes
- *(hermes)* resolve session projects through registry
- *(hermes)* route projectless compression to user store
- harden runtime routing and ci

### Other

- restore structured backfill process state
- canonicalize registry alias fixtures

## [0.0.58](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.57...v0.0.58) - 2026-07-11

### Fixed

- *(windows)* skip unsupported migration runtime
- *(windows)* harden migration recovery tests
- *(windows)* exercise migration recovery
- *(windows)* secure lifecycle handoff

### Other

- *(windows)* exercise consolidation suite
- *(windows)* isolate offline migration locks
- *(windows)* skip unsupported consolidation suite

## [0.0.57](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.56...v0.0.57) - 2026-07-11

### Fixed

- *(windows)* handle platform-specific runtime behavior
- *(daemon)* gate unix shutdown internals
- *(sessions)* harden user routing and daemon shutdown

### Other

- Merge remote-tracking branch 'origin/master' into fix/user-message-search-storage-scope

## [0.0.56](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.55...v0.0.56) - 2026-07-11

### Fixed

- *(hermes)* isolate projectless host routing ([#445](https://github.com/ScriptedAlchemy/tracedecay/pull/445))

## [0.0.55](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.54...v0.0.55) - 2026-07-11

### Fixed

- *(agents)* recover post-update integration state

## [0.0.54](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.53...v0.0.54) - 2026-07-11

### Added

- *(memory)* add autonomous user-scoped learning
- *(memory)* add transactional graph grooming
- *(automation)* review Hermes terminal receipts
- *(automation)* apply safe skill consolidations
- *(hermes)* export managed skills through plugin discovery
- *(automation)* wake reviews from fresh session activity
- *(memory)* add profile-level user scope

### Fixed

- *(storage)* preserve fact relations in consolidation
- *(memory)* bound and validate graph grooming
- *(storage)* preserve branch snapshot recovery files
- *(hermes)* gate receipt reviews on ingested turns
- *(storage)* snapshot live branch databases safely
- *(sessions)* backfill Hermes turns by project evidence
- *(hermes)* correlate live turns with projects
- *(mcp)* avoid panic in memory status schema
- *(hermes)* configure all profile providers
- *(hermes)* isolate memory routes and context clones
- *(hermes)* route memory by session workspace
- *(hermes)* guard plugin nudge and memory retrieval

### Other

- *(automation)* restore guarded skill consolidation
- *(automation)* keep skill consolidations staged

## [0.0.53](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.52...v0.0.53) - 2026-07-10

### Fixed

- *(doctor)* isolate registry diff conflicts ([#440](https://github.com/ScriptedAlchemy/tracedecay/pull/440))
- *(doctor)* derive orphan stores from registry reconstruction ([#439](https://github.com/ScriptedAlchemy/tracedecay/pull/439))
- *(storage)* retire applied consolidation inputs ([#438](https://github.com/ScriptedAlchemy/tracedecay/pull/438))
- *(db)* keep FTS repair out of search reads ([#435](https://github.com/ScriptedAlchemy/tracedecay/pull/435))
- *(sqlite)* disable graph mmap across peer checkpoints ([#436](https://github.com/ScriptedAlchemy/tracedecay/pull/436))

## [0.0.52](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.51...v0.0.52) - 2026-07-10

### Fixed

- *(storage)* make registry reconstruction conflict-safe ([#434](https://github.com/ScriptedAlchemy/tracedecay/pull/434))
- *(hooks)* honor lifecycle quiescence

## [0.0.51](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.50...v0.0.51) - 2026-07-10

### Other

- *(storage)* index consolidation message lookups

## [0.0.50](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.49...v0.0.50) - 2026-07-10

### Fixed

- *(storage)* preserve divergent session variants

## [0.0.49](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.48...v0.0.49) - 2026-07-10

### Fixed

- *(storage)* recover untracked branch graphs

## [0.0.48](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.47...v0.0.48) - 2026-07-10

### Added

- *(mcp)* refresh tools after daemon generation change ([#422](https://github.com/ScriptedAlchemy/tracedecay/pull/422))

### Fixed

- *(storage)* add explicit split-store consolidation ([#425](https://github.com/ScriptedAlchemy/tracedecay/pull/425))
- *(analytics)* aggregate sections before sampling ([#424](https://github.com/ScriptedAlchemy/tracedecay/pull/424))
- *(memory)* preserve FTS5 relevance direction ([#423](https://github.com/ScriptedAlchemy/tracedecay/pull/423))
- *(hermes)* use the user TraceDecay profile ([#407](https://github.com/ScriptedAlchemy/tracedecay/pull/407))
- *(runtime)* proxy MCP before opening local stores ([#420](https://github.com/ScriptedAlchemy/tracedecay/pull/420))
- *(edit)* make move_symbol writes race-safe ([#419](https://github.com/ScriptedAlchemy/tracedecay/pull/419))
- *(doctor)* surface identity split conflicts ([#417](https://github.com/ScriptedAlchemy/tracedecay/pull/417))

### Other

- Merge pull request #414 from ScriptedAlchemy/codex/move-symbol
- Merge pull request #411 from ScriptedAlchemy/codex/doctor-foreign-skills

## [0.0.47](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.46...v0.0.47) - 2026-07-10

### Fixed

- *(sessions)* collapse copied subagent prompts ([#410](https://github.com/ScriptedAlchemy/tracedecay/pull/410))

## [0.0.46](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.45...v0.0.46) - 2026-07-10

### Fixed

- *(runtime)* drain daemon safely during upgrades
- *(storage)* preserve detached worktree isolation
- *(storage)* recognize linked-worktree legacy stores
- *(storage)* adopt legacy identity stores safely
- *(codex)* validate preserved marketplace identity
- *(hooks)* classify trusted compiler failure evidence
- harden hook trust and hint integrity
- *(cursor)* brand plugin MCP as tracedecay (not graph) ([#400](https://github.com/ScriptedAlchemy/tracedecay/pull/400))

### Other

- Merge pull request #405 from ScriptedAlchemy/codex/legacy-store-adoption

## [0.0.45](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.44...v0.0.45) - 2026-07-09

### Added

- add investigating-unexpected-changes skill + hint

### Fixed

- *(mcp)* anchor PR context at merge base
- *(skills)* reconcile bundled skill registries
- narrow force-push hint triggers to possessive confusion forms

### Other

- *(runtime)* keep process identity API minimal ([#398](https://github.com/ScriptedAlchemy/tracedecay/pull/398))
- *(mcp)* tree-sitter masked source for text scanners ([#391](https://github.com/ScriptedAlchemy/tracedecay/pull/391))
- Merge pull request #395 from ScriptedAlchemy/codex/post-wave-cleanup
- *(dashboard)* isolate fixtures from session catch-up ([#394](https://github.com/ScriptedAlchemy/tracedecay/pull/394))
- *(git-watch)* observe exact debounce drains ([#393](https://github.com/ScriptedAlchemy/tracedecay/pull/393))
- *(skills)* validate branch context against live Git

## [0.0.44](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.43...v0.0.44) - 2026-07-09

### Added

- *(plugins)* add MCP-optional capability discovery ([#368](https://github.com/ScriptedAlchemy/tracedecay/pull/368))
- daemon auto-tracking of open PR branches

### Fixed

- *(sessions)* deduplicate Claude reasoning text ([#384](https://github.com/ScriptedAlchemy/tracedecay/pull/384))
- *(mcp)* cover all standard format captures ([#383](https://github.com/ScriptedAlchemy/tracedecay/pull/383))
- *(skills)* harden managed-skill materialization
- *(pr-autotrack)* harden daemon PR-branch auto-tracking ([#377](https://github.com/ScriptedAlchemy/tracedecay/pull/377))
- *(backfill)* make structured-row sweep process-safe
- *(sessions)* distinguish produced and observed commits ([#369](https://github.com/ScriptedAlchemy/tracedecay/pull/369))
- *(daemon)* reconcile owned PR tracking safely ([#367](https://github.com/ScriptedAlchemy/tracedecay/pull/367))
- *(skills)* preserve materialization ownership safely ([#366](https://github.com/ScriptedAlchemy/tracedecay/pull/366))

### Other

- hash reconcile artifacts once, reuse shared sha256 helper
- Merge pull request #376 from ScriptedAlchemy/codex/attribution-hardening
- Merge pull request #375 from ScriptedAlchemy/codex/lcm-grep-cap-disclosure
- Merge pull request #374 from ScriptedAlchemy/codex/backfill-process-safety

## [0.0.43](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.42...v0.0.43) - 2026-07-09

### Fixed

- restore test attribute placement in message_noise
- bare code fences no longer count as work evidence
- precompute inventory key and guard branch-inventory over-match
- *(sessions)* hoist listing indicators above statements for clippy
- *(sessions)* widen fetch instead of panicking on large limits
- *(sessions)* downrank inventory noise in message_search ranking

### Other

- deslop message noise followups

## [0.0.42](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.41...v0.0.42) - 2026-07-09

### Fixed

- *(storage)* preserve symlink guard under macOS temp roots
- *(storage)* scope symlink guard to the final path component
- *(graph)* reject external qualified calls, prune lcm table
- *(graph)* resolver prefilter, scheduler hardening, security batch

## [0.0.41](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.40...v0.0.41) - 2026-07-09

### Added

- *(sessions)* ingest and surface Codex thread goals

### Fixed

- *(db)* stop conflating attrs_start_line 0 with unset

### Other

- Merge pull request #347 from ScriptedAlchemy/codex/fix-unsafe-patterns
- Merge pull request #340 from ScriptedAlchemy/codex/session-recovery-fixes
- Merge pull request #346 from ScriptedAlchemy/codex/goal-ingestion

## [0.0.40](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.39...v0.0.40) - 2026-07-09

### Added

- compact tool telemetry ingestion across Codex, Claude Code, and Cursor ([#325](https://github.com/ScriptedAlchemy/tracedecay/pull/325))

### Fixed

- *(config)* atomic user-config saves with honest errors and recovery
- eliminate three CI test flakes at their roots

### Other

- Merge pull request #322 from ScriptedAlchemy/codex/project-discovery-rendering
- Merge remote-tracking branch 'origin/master' into codex/project-discovery-rendering-fixes
- Merge pull request #338 from ScriptedAlchemy/codex/automation-resilience

## [0.0.39](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.38...v0.0.39) - 2026-07-08

### Fixed

- lock private jsonl appends ([#323](https://github.com/ScriptedAlchemy/tracedecay/pull/323))

## [0.0.38](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.37...v0.0.38) - 2026-07-08

### Fixed

- *(hooks)* broaden hint eval coverage
- *(hooks)* compact TraceDecay hook steering

### Other

- Merge pull request #318 from ScriptedAlchemy/release-plz-2026-07-08T00-48-50Z
- *(hooks)* make hint eval coverage explicit
- *(hooks)* table-drive hint eval cleanup
- *(hooks)* centralize hint category metadata
- simplify hook eval helpers
- *(hooks)* deslop hint classifier changes
- *(hooks)* share shell command parsing
- *(hooks)* deslop hint eval coverage
- *(hooks)* expand hint scenario evals

## [0.0.37](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.36...v0.0.37) - 2026-07-08

### Fixed

- simplify autonomous memory curation

### Other

- remove dead legacy automation config knobs

## [0.0.36](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.35...v0.0.36) - 2026-07-07

### Fixed

- *(cursor)* ignore stale degraded MCP ambiguity logs

## [0.0.35](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.34...v0.0.35) - 2026-07-07

### Fixed

- *(upgrade)* clarify missing release asset state

## [0.0.34](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.33...v0.0.34) - 2026-07-07

### Added

- install codex managed agents
- *(codex)* install proactive memory prompt rules
- *(memory)* document proactive fact-store guidance
- *(mcp)* improve tool output rendering
- *(memory)* reinforce recalled fact ranking
- add memory feedback trust scenario
- capture default timing telemetry
- make the worldwide token-savings counter opt-in

### Fixed

- *(release)* guard against unpublished version drift
- provider-neutral ingest guidance
- *(mcp)* normalize read symbol context lines
- *(claude)* validate marketplace metadata
- refresh Codex managed agent overlays
- address automation cleanup review findings
- satisfy renderer clippy lint
- restore skill renderer module
- stabilize automation cleanup CI
- *(diagnostics)* improve freshness handling
- satisfy clippy large-future lints
- route init to worktree-local index

### Other

- clarify repo-local cargo targets
- simplify README
- Fix Codex hook manifest schema
- release v0.0.34
- simplify diagnostics and renderer fixes
- split MCP render and diagnostics boundaries
- Merge remote-tracking branch 'origin/codex/lazy-ignored-dependency-indexing' into simplify/automation-mcp-cleanup
- split mcp renderer tests
- *(evals)* document the fact-store triggering/adoption scorecard
- behavior-preserving cleanups from the merged #295 changes

## [0.0.33](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.32...v0.0.33) - 2026-07-06

### Fixed

- *(codex)* silence plugin-only MCP cleanup ([#294](https://github.com/ScriptedAlchemy/tracedecay/pull/294))

### Other

- Fix automation self-improvement review findings

## [0.0.32](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.31...v0.0.32) - 2026-07-05

### Added

- add repo-local TraceDecay usage skills ([#292](https://github.com/ScriptedAlchemy/tracedecay/pull/292))

## [0.0.31](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.30...v0.0.31) - 2026-07-05

### Other

- *(mcp)* split session tool definitions ([#290](https://github.com/ScriptedAlchemy/tracedecay/pull/290))

## [0.0.30](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.29...v0.0.30) - 2026-07-05

### Added

- *(sessions)* index workflow runs and agents (layer 2 of session intelligence) ([#284](https://github.com/ScriptedAlchemy/tracedecay/pull/284))
- *(plugin)* add plugin-suite improvements
- *(sessions)* add git-anchored session correlation
- *(claude)* add plugin namespace permissions
- *(sessions)* add date filters to recall search ([#275](https://github.com/ScriptedAlchemy/tracedecay/pull/275))

### Fixed

- migrate Codex plugin cache to personal namespace ([#282](https://github.com/ScriptedAlchemy/tracedecay/pull/282))

### Other

- Support stdin for tool args ([#286](https://github.com/ScriptedAlchemy/tracedecay/pull/286))
- *(git-watch)* make auto-sync watcher tests deterministic ([#285](https://github.com/ScriptedAlchemy/tracedecay/pull/285))
- isolate install-family tests from process-global env bleed ([#283](https://github.com/ScriptedAlchemy/tracedecay/pull/283))
- split oversized merged modules ([#287](https://github.com/ScriptedAlchemy/tracedecay/pull/287))
- configure Cargo scratch build paths

## [0.0.29](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.28...v0.0.29) - 2026-07-04

### Added

- *(claude)* sync Claude Code plugin cache on install and update ([#273](https://github.com/ScriptedAlchemy/tracedecay/pull/273))

### Fixed

- *(sessions)* follow store remote fallback for renamed checkouts ([#269](https://github.com/ScriptedAlchemy/tracedecay/pull/269))
- *(lsp)* classify initialize-request write failures with stderr ([#270](https://github.com/ScriptedAlchemy/tracedecay/pull/270))
- *(claude)* write schema-required marketplace registration fields ([#268](https://github.com/ScriptedAlchemy/tracedecay/pull/268))

## [0.0.28](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.27...v0.0.28) - 2026-07-04

### Fixed

- *(agents)* memory digest export lifecycle ([#266](https://github.com/ScriptedAlchemy/tracedecay/pull/266))

## [0.0.27](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.26...v0.0.27) - 2026-07-04

### Other

- [codex] Improve Codex plugin hook trust doctor ([#258](https://github.com/ScriptedAlchemy/tracedecay/pull/258))

## [0.0.26](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.25...v0.0.26) - 2026-07-04

### Added

- *(analytics)* adoption analytics, diagnostics skill, hook replay ([#260](https://github.com/ScriptedAlchemy/tracedecay/pull/260))

### Fixed

- *(automation)* satisfy clippy in job_webhook reset tests ([#261](https://github.com/ScriptedAlchemy/tracedecay/pull/261))
- *(jobs)* treat a webhook reset after a full response as delivered ([#259](https://github.com/ScriptedAlchemy/tracedecay/pull/259))

### Other

- Default TraceDecay tool output to markdown ([#262](https://github.com/ScriptedAlchemy/tracedecay/pull/262))
- *(plugin)* deslop merged plugin cleanup ([#256](https://github.com/ScriptedAlchemy/tracedecay/pull/256))

## [0.0.25](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.24...v0.0.25) - 2026-07-03

### Added

- *(plugin)* embed skill trees recursively + ship first support files
- *(hooks)* inject adoption contract on Claude and Kiro
- *(claude)* ship a Claude Code plugin bundle + sync skills

### Fixed

- *(build)* fail codegen clearly on a non-UTF-8 support file
- *(kiro)* recognize the legacy steering marker on lifecycle ops
- *(cursor)* sweep retired skills from the live bundle
- *(claude)* escape hook path, settings guards, clean replace
- *(plugin)* retarget cursor legacy-sweep test at retired skills
- *(skills)* scope legacy-block fallback to single-host uninstall
- *(skills)* remove staged overlay dir on failed swap
- *(lsp)* return diagnostics for suppress-empty servers
- *(jobs)* block IPv4-mapped/embedded IPv6 webhook SSRF targets
- address Claude review findings
- *(lsp)* relax initialize timeout floor

### Other

- dedup installer helpers and simplify marked-block splicing ([#254](https://github.com/ScriptedAlchemy/tracedecay/pull/254))
- *(git)* resolve git binary once via cached git_program() ([#253](https://github.com/ScriptedAlchemy/tracedecay/pull/253))
- *(hooks)* make hook_events git spawns resilient under load
- *(cursor)* seed retired-skill sweep tests with tracedecay markers
- *(plugin)* merge memory skills + split message_search lanes
- *(plugin)* re-express cursor dispatchers as native slash commands
- *(plugin)* collapse host bundles into one plugin/ tree
- speed up slow junit cases

## [0.0.24](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.23...v0.0.24) - 2026-07-03

### Added

- inject fact-store memory into Codex and Cursor hook context ([#229](https://github.com/ScriptedAlchemy/tracedecay/pull/229))
- *(automation)* combined reflector+skill scheduler pass (Hermes G9/R7)
- *(cli)* enforce MCP tool CLI/skill parity and rich subcommand help

### Fixed

- *(lcm)* default transcript reads to all providers
- *(sessions)* advance Claude cursor for filtered transcripts ([#241](https://github.com/ScriptedAlchemy/tracedecay/pull/241))
- *(lcm)* run DB-side GC phases when the payload dir is missing
- *(sessions)* preserve worktree routing metadata
- *(ci)* harden cross-worktree provenance tests
- *(sessions)* route cross-worktree provenance
- *(sessions)* preserve transcript location metadata
- *(codex)* preserve branch context in transcript metadata
- *(agents)* require tracedecay local exports
- *(lsp)* bound diagnostics refresh hangs
- *(sessions)* normalize activity timestamps
- satisfy clippy pedantic lints in windows read retry
- *(sync)* retry transient Windows file locks in read_source_file

### Other

- *(mcp)* isolate dashboard fixture storage
- Merge remote-tracking branch 'origin/master' into codex/export-approved-skills
- Merge remote-tracking branch 'origin/master' into codex/hermes-parity-r9-user-jobs
- Merge remote-tracking branch 'origin/master' into codex/skill-adoption-babysit
- Merge remote-tracking branch 'origin/master' into codex/hermes-parity-r8-memory-digest
- Merge remote-tracking branch 'origin/master' into codex/mcp-connection-root-routing
- Merge remote-tracking branch 'origin/master' into codex/hook-session-route
- *(sessions)* clarify activity timestamp units
- Merge pull request #218 from ScriptedAlchemy/codex/activity-coupled-triggering
- Merge pull request #226 from ScriptedAlchemy/codex/hermes-parity-r10-outcomes
- Merge pull request #213 from ScriptedAlchemy/codex/tool-skill-cli-parity
- *(cli)* trim tool help test commentary

### Added

- Claude Code installs now register `SessionStart` and `PostToolUse` lifecycle hooks, matching the freshness/steering coverage Cursor, Codex, and Kiro already had: `SessionStart` reports index freshness and injects the LCM context-recovery hint after compaction; `PostToolUse` notifies the daemon for targeted incremental sync after edits and shell commands. Existing installs pick the hooks up via the post-upgrade backfill or `tracedecay doctor`.
- The CLI-fallback steering ("if MCP fails, use `tracedecay tool ...`") now reaches every host with a prompt-rules surface — Claude Code, Copilot/VS Code, Gemini, OpenCode, Kimi, Vibe, and Kiro — instead of only the Cursor rule and Codex session hook.

### Fixed

- **`serve` no longer exits when project resolution fails at startup** — MCP hosts (Cursor especially) never retry a failed server spawn, so one startup exit over a recoverable config problem (uninitialized project, ambiguous global fallback, bad `--path`) turned every later tool call in the session into "Timed out waiting for connection". `serve` now stays alive in a degraded mode: it completes the MCP handshake, lists the real tools, and answers each tool call with an actionable error naming the failure, the fix, and the `tracedecay tool …` CLI fallback. It rechecks the project on every tool call and recovers in-session once `tracedecay init` (or a corrected path) makes resolution succeed — no server toggle or window reload needed.
- **`serve` now tolerates a literal unexpanded `--path ${workspaceFolder}`** — Cursor's headless agent-session MCP scopes spawn the plugin's serve command without expanding the template variable and never retry the failed scope, which surfaced as "Timed out waiting for connection" on every tool call. `serve` now discards an unexpanded `${...}` template value with a stderr warning and falls back to project discovery where possible, requiring a unique registered project when discovery reaches the global registry in this mode. Rationale and details in `cursor-plugin/README.md`.

### Added

- **`tracedecay doctor --agent cursor` now diagnoses dead Cursor MCP scopes** — best-effort scan of Cursor's recent MCP logs for tracedecay spawn failures (literal unexpanded `${workspaceFolder}` paths, `Connection failed: MCP error -32000`, degraded-mode notices) with concrete remediation ("toggle the MCP server in Cursor Settings → MCP or reload the window"), plus a plugin-bundle-version-vs-binary-version staleness check that points at `tracedecay update-plugin`.

## [0.0.23](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.22...v0.0.23) - 2026-07-02

### Other

- Merge pull request #204 from ScriptedAlchemy/codex/winperf-flakes-20260702

## [0.0.22](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.21...v0.0.22) - 2026-07-02

### Other

- *(test)* cut hidden per-test costs in lib and bin unit suites

## [0.0.21](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.20...v0.0.21) - 2026-07-02

### Fixed

- ride out daemon restarts in MCP daemon clients

### Other

- Merge remote-tracking branch 'origin/master' into codex/self-improve-20260701
- Merge remote-tracking branch 'origin/master' into codex/update-daemon-reconnect
- Merge pull request #173 from ScriptedAlchemy/codex/daemon-incremental-sync
- address plugin skill contract review findings
- add canonical skill frontmatter parser
- add plugin skill contract checks

### Other

- isolate the test suite from the real `~/.tracedecay` profile by default

## [0.0.20](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.19...v0.0.20) - 2026-07-01

### Other

- trim more Windows hotspot fixtures

## [0.0.19](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.18...v0.0.19) - 2026-07-01

### Fixed

- dedupe tool descriptor hints separately
- strengthen TraceDecay tool discovery hints
- handle daemon hook review followups
- resolve worktree registry context by identity
- prefer crates.io for cargo updates

### Other

- daemon-owned hook event notifications
- [codex] Fix automation retry and fact curation policy ([#161](https://github.com/ScriptedAlchemy/tracedecay/pull/161))
- Simplify session provider ingest selection
- scope message search catch-up
- speed up Windows shard fixtures

## [0.0.18](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.17...v0.0.18) - 2026-07-01

### Added

- add daemon dashboard project selector

### Fixed

- refresh selected project cache
- harden dashboard project selection
- reject scoped project mutations before state load
- isolate selected dashboard project state

### Other

- Merge remote-tracking branch 'origin/master' into codex/tracedecay-followup-diagnostics
- trim Windows fixture overhead
- simplify selected project cache
- speed up windows rust checks ([#148](https://github.com/ScriptedAlchemy/tracedecay/pull/148))

## [0.0.17](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.16...v0.0.17) - 2026-06-29

### Other

- Add TraceDecay registry, LCM, and transcript audit improvements ([#142](https://github.com/ScriptedAlchemy/tracedecay/pull/142))

## [0.0.16](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.15...v0.0.16) - 2026-06-29

### Fixed

- allow project selectors by unique basename
- improve daemon telemetry and automation parsing

## [0.0.15](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.14...v0.0.15) - 2026-06-27

### Fixed

- resolve profile stores in daemon tool calls

## [0.0.14](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.13...v0.0.14) - 2026-06-27

### Fixed

- handle dynamic tool help

## [0.0.13](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.12...v0.0.13) - 2026-06-26

### Added

- add dashboard automation controls
- add dashboard automation APIs
- expose automation through mcp tools
- add self-improving automation foundation
- support linked worktree store resolution

### Fixed

- avoid lazy sync for read-only MCP tools
- refresh Hermes dashboard automation routes
- reject duplicate managed skill drafts
- dedupe mcp skill view analytics
- preserve automation clears and run ids
- catalog Codex goal context response items
- preserve serve remote store fallback

### Other

- simplify PR 131 wording
- document self-improving automation loops
- simplify dashboard skill api errors
- simplify managed skill mcp handler lookups
- tune Windows CI runtime
- lower precompact summary cap
- harden Windows CI timing paths
- Merge pull request #121 from spa5k/feat/worktree-native-store-resolution
- share git common dir resolution
- document linked worktree store resolution

### Added

- support linked git worktrees resolving the initialized repository store by git common directory

## [0.0.12](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.11...v0.0.12) - 2026-06-24

### Fixed

- avoid WAL teardown on Windows ([#118](https://github.com/ScriptedAlchemy/tracedecay/pull/118))
- disable graph DB mmap before WAL ([#117](https://github.com/ScriptedAlchemy/tracedecay/pull/117))
- disable GlobalDb mmap on Windows ([#115](https://github.com/ScriptedAlchemy/tracedecay/pull/115))

## [0.0.11](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.10...v0.0.11) - 2026-06-24

### Added

- make update refresh plugins and daemon

### Fixed

- simplify compact path list rendering
- keep daemon MCP proxy alive across restarts
- *(db)* disable SQLite mmap on Windows to stop teardown crash

## [0.0.10](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.9...v0.0.10) - 2026-06-24

### Fixed

- satisfy clippy on profile branch
- handle unresolved profile store lookups

### Other

- remove redundant clippy suppressions
- Centralize profile-scoped branch tracking

## [0.0.9](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.8...v0.0.9) - 2026-06-23

### Other

- Fix Hermes JSON dispatch and Windows fixture teardown
- Merge master into MCP response rendering
- Return structured clean commit context
- Deslop MCP response rendering
- Allow Cursor subagents with TraceDecay installed
- Compact remaining machine-tool JSON for format consistency
- Green the suite after MCP markdown-default change
- Document MCP response format changes
- Add markdown rendering for MCP tool responses

### Changed

- **Markdown is now the default MCP tool output format.** Read/list/analysis/context tools (≈70 tools across `search`, `callers`, `callees`, `impact`, `outline`, `body`, `status`, `complexity`, `hotspots`, `health`, `test_map`, `pr_context`, …) now return compact markdown — bullets and GitHub-flavored tables — instead of pretty-printed JSON. Markdown is denser (no per-row key repetition, no brace/indentation overhead), scans better for models, and pushes responses away from the 15K-char truncation cliff. Symbol identifiers (`node_id`, `qualified_name`, `signature`) are preserved inline in backticks so follow-up calls (`body`/`callers`/`callees`) still chain cleanly. `tracedecay_context` was already markdown and is unchanged for the default path.
- **New `format` argument** on every markdown-capable tool: pass `format: "json"` to get compact machine-readable JSON (for programmatic consumers); the default is `format: "markdown"`. Unrecognized values fall back to markdown.
- **JSON output is never pretty-printed anymore.** Tools that intentionally stay JSON (edit primitives, `dashboard`, `fact_store`/`fact_feedback`, retrieval handles, and the LCM/session lifecycle tools) now emit compact `serde_json::to_string` rather than `to_string_pretty`, a ~30–40% byte reduction with no semantic change. `tracedecay_files` (grouped/flat text) and `tracedecay_type_hierarchy` (text tree) already returned dense text and are unchanged.
- Shared `src/mcp/tools/render.rs` module centralizes format selection, format-aware truncation, and the generic JSON→markdown renderer used by tools without a bespoke layout.

## [0.0.8](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.7...v0.0.8) - 2026-06-23

### Other

- Address aggregate review follow-ups
- Stabilize hook hint dedupe across layout races
- Stabilize merged PR stack
- Honor git work-tree after -C in hooks
- Record hook hints and steer new subagents

## [0.0.7](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.6...v0.0.7) - 2026-06-22

### Fixed

- reset codex compaction depth on replay

## [0.0.6](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.5...v0.0.6) - 2026-06-21

### Added

- add compression context recovery hints

### Fixed

- fix Windows global registry test races
- fix registry repo identity matching
- fix cleanup policy docs
- fix Hermes profile storage routing
- fix Hermes profile storage routing
- fix dashboard store and savings calculations
- fix plugin contract tests
- fix storage registry and session routing

### Other

- remove legacy daemon names
- Clean up CI docs and legacy naming
- remove legacy Hermes provider alias
- Update Hermes and plugins for unified stores
- Update dashboard for unified stores
- Add unified storage session backends

## [0.0.5](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.4...v0.0.5) - 2026-06-20

### Fixed

- *(dashboard)* refresh shell source stamp
- *(branch)* harden auto tracking under contention
- *(branch)* auto-track active tracedecay branches
- *(dashboard)* tighten dist freshness checks
- *(build)* refresh dashboard deps during auto rebuilds

### Other

- *(dashboard)* trim primitive comments
- *(dashboard)* real React imports, stale-dist build guard

## [0.0.4](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.3...v0.0.4) - 2026-06-20

### Fixed

- *(migrate)* verify sqlite artifacts logically
- *(storage)* stabilize profile storage workflows
- *(storage)* add profile storage foundations

## [0.0.3](https://github.com/ScriptedAlchemy/tracedecay/compare/v0.0.2...v0.0.3) - 2026-06-19

### Fixed

- *(deps)* remove unused dependencies

### Other

- stabilize Windows GC test and cancel stale runs


## [0.0.2] - 2026-06-18

### Changed
- **Project renamed to TraceDecay.** The crate, binary, CLI command, and MCP server are now `tracedecay`, and the MCP tools are prefixed `tracedecay_*`. The data directory is now `.tracedecay/`, and environment variables use the `TRACEDECAY_*` prefix. All entries below predate the rename and intentionally retain their historical names.
- **Version reset for the rebranded crate.** `tracedecay` now restarts at `0.0.2` as a fresh crate line; `0.0.1` is already occupied on crates.io by the name-reservation placeholder, so `0.0.2` is the first publishable release number.

### Added
- **Local web dashboard (`tracedecay dashboard` + `tracedecay_dashboard` MCP tool).** A self-contained axum server with compile-time-embedded UI assets serving three tabs: **Holographic Memory** (fact/entity/bank inspector, 2D PCA semantic map, association graph, phase-cosine similarity explorer with brushable histogram, and feature-flagged curation), **LCM** (overview, FTS search with role/source/session facets, session/node drilldowns, timeline, compression analytics over the global DB), and **Code Graph** (overview analytics plus a force-directed canvas explorer with search-to-focus, progressive neighbor expansion, callers/callees, filters, and shortest-path mode). CLI flags: `--path`, `--host`, `--port` (0 = auto, parseable URL on stdout), `--open`. The `tracedecay_dashboard` MCP tool starts/stops the same server as a background task and returns the URL. `GET /api/capabilities` advertises feature flags (`memory`, `lcm`, `graph`, `curation`, `llm_curation`) for host/UI feature detection. Dark + light themes, responsive down to ~420px.
- **Memory curation with hard-delete semantics.** `POST /api/plugins/holographic/curate` proposes (dry-run) or applies similarity-based deduplication: the lower-trust fact of each `likely_duplicate` pair is permanently deleted via the canonical store path (FK-cascaded entity links, FTS trigger cleanup, bank dirty-marking) — no archive state, no restore. `POST /curate/apply` exposes a generic delete/merge ops contract for external (e.g. LLM-backed) planners; per-op failures are reported per-op. Migration v13 only cleans up a never-shipped archive-column experiment from local dev databases.
- **Hermes wrapper for the dashboard.** `dashboard/hermes-wrapper/` (canonical; deployed to the hermes-agent working tree) reverse-proxies `/holographic/*`, `/lcm/*`, and `/graph/*` to a spawned or externally configured tracedecay dashboard, re-hosts the same UI bundles under the combined "TraceDecay" tab, layers an optional LLM curation planner on the `/curate/apply` contract, and hardens the subprocess lifecycle (stderr drain, parent-death signal, spawn-failure backoff).
- **Dashboard build + test infrastructure.** `dashboard/` npm workspace (esbuild) building all bundles, 16 frontend unit tests (`node run-unit-tests.mjs`), Playwright smoke (`npm run smoke -- --expect-lcm=empty|non-empty`), Rust integration suites (`tests/dashboard_api_test.rs`, `dashboard_lcm_fixes_test.rs`, `dashboard_graph_api_test.rs`, `mcp_dashboard_tool_test.rs`), a dashboard CI job, and `build.rs` rerun-if-changed guards so frontend dist changes force re-embedding.
- **Curation previews survive dashboard restarts.** The last dry-run curation plan is mirrored to a `.tracedecay/dashboard/curation_preview.json` sidecar and re-hydrated when the server starts; applying curation (or any `/curate/apply` mutation) clears both the in-memory copy and the sidecar. The `GET /curation/preview` API shape is unchanged, and staleness is still recomputed against the live fact count on every read.
- **`tracedecay install --agent hermes` deploys the dashboard plugin page.** The Hermes wrapper (manifest, `plugin_api.py` reverse proxy, and the UI bundles — all embedded in the binary, no source checkout needed) is now written to `<hermes_home>/plugins/tracedecay/dashboard/` as part of the default install, where Hermes' dashboard-plugin discovery (stock and forked) picks it up as a "TraceDecay" tab with Memory / LCM / Code Graph / Savings sub-tabs. The deployed proxy bakes in the installing binary path and the profile's pinned `project_root` as spawn-mode defaults (`TRACEDECAY_BIN` / `TRACEDECAY_DASHBOARD_PROJECT` env vars still win); reinstalls preserve the pin, `--no-dashboard` opts out (and removes a previous deploy), and uninstall cleans the page up. The wrapper also gained the Savings sub-tab (`/savings/*` proxy to `/api/plugins/savings/*`). On Hermes versions without dashboard-plugin discovery the deployed directory is inert.
- **Dashboard assets build themselves on fresh checkouts.** When the embedded `dashboard/*/dist` bundles are missing, `build.rs` now runs the frontend build automatically (`npm ci`, falling back to `npm install`, then `npm run build`) with progress reported as build warnings — so `cargo build` / `cargo install --path .` work from a clean clone. If npm is unavailable, the build still fails fast with actionable instructions. `Cargo.toml` switched to an explicit `package.include` whitelist that ships the prebuilt dist bundles inside the crate package, making `cargo package`/`cargo publish` verifiable and letting crates.io/docs.rs builds proceed with no Node.js toolchain. The release workflows (`release.yml` build + publish-crate jobs, `release-beta.yml`) gained the same dashboard prebuild step as CI.

- **Tokenizer-backed cost tier for the Savings & Cost tab (`token-counting` feature, on by default).** When transcripts carry no usage counters (all Cursor stores — verified to contain none — plus cline/vibe and any Codex/Claude rows without usage), stored message text is now counted with a real BPE tokenizer (tiktoken-rs, `o200k_base`/`cl100k_base`) instead of the chars/4 heuristic: exact for OpenAI-family models, a labeled `≈` approximation for vendors without a public tokenizer (Claude/Gemini). The API gains a third `cost_basis` value `"tokenized"` (between `"actual"` and `"estimated"`; `"mixed"` semantics unchanged), additive `tokenized` token blocks, `tokenized_messages` counts, and per-model `tokenizer` provenance (`{"encoder", "exact"}`); the UI shows tier badges and an updated methodology note. Counts are cached per message (in-process map + a `dashboard_token_counts` sidecar table in the global accounting DB, keyed by message identity with a text-length guard) and pre-warmed in the background at dashboard startup, so 15k+-message stores pay the BPE pass once instead of per request. Disable the feature for a leaner binary (~4 MB embedded vocabularies, lazily decoded) — everything degrades to the chars/4 tier.

### Fixed
- **The savings ledger records by default again — the Savings tab is no longer empty while lifetime counters grow.** The holographic-fact-store commit made the MCP server's global accounting DB opt-in via `TRACEDECAY_ENABLE_GLOBAL_DB`, which silently disabled `savings_ledger` writes (and worldwide-counter flushes) for every default install: tool calls still printed `tracedecay_metrics` lines and CLI paths kept growing `projects.tokens_saved`, but the dashboard showed "ledger calls: 0 / no events yet". Global accounting is now **on by default**; opt out with `TRACEDECAY_DISABLE_GLOBAL_DB=1` (set automatically for cargo-launched processes via `.cargo/config.toml` so test runs stay hermetic) or `TRACEDECAY_ENABLE_GLOBAL_DB=0`, with an explicit `TRACEDECAY_ENABLE_GLOBAL_DB=1` always winning. The dashboard now also surfaces the gate verdict (`savings.recording` in the overview API, a `recording: on/off` badge, and an honest explanation when the ledger is empty — including the "restart your MCP server to pick this up" case) instead of an unconditional "no events yet". Covered by a default-on ledger regression test plus env-precedence unit tests; long-running MCP servers must be restarted/reloaded to pick up the fix.
- **Hermes wrapper spawn mode no longer drops its child server after idle periods.** The wrapper's Linux parent-death guard (`PR_SET_PDEATHSIG`) fires when the *thread* that forked the child exits — and FastAPI sync endpoints run on anyio threadpool workers that are reaped after ~10s idle, so the spawned `tracedecay dashboard` was SIGTERMed shortly after quiet spells (surfacing as intermittent 502 "connection reset by peer" on the next tab click). `plugin_api.py` now spawns from a single long-lived worker thread, binding the child's lifetime to the Hermes host process as intended.
- **Hermes wrapper cold starts no longer 502.** After spawning, the wrapper now waits (bounded, 30s) for the engine's `/api/capabilities` to answer before proxying the first request, returns a clear `503` with `Retry-After` if the engine truly fails to come up, and transparently retries GET proxies once after re-resolving the upstream (which reaps and respawns a dead child). POSTs are never retried so curation applies cannot run twice.
- **Fallback branch DBs are now read-only for sync/index writes.** `tracedecay sync`, lazy single-file syncs, and full indexing now refuse to write when the active git branch is being served from an ancestor branch database, preventing branch-only files from being indexed into the fallback DB.
- **`tracedecay install --agent hermes` generates a plugin that loads on newer Hermes hosts.** Four generator/installer fixes: (1) the generated `TraceDecayContextEngine` implements the now-abstract `update_from_response(usage)` method (normalizes `prompt/input`, `completion/output`, and `total` token counts into `last_*_tokens` attributes), so plugin load no longer dies with `Can't instantiate abstract class`; (2) the skill registers under the bare name `tracedecay` — newer Hermes derives the namespace from the plugin and rejects `:` in skill names; (3) the installer now matches the existing indentation of `plugins.enabled`/`plugins.disabled` lists (Hermes writes 2-space items) instead of always inserting 4-space items, which produced unparseable mixed-indent YAML; and (4) flow-style empty lists (`disabled: []`, which Hermes itself writes) are accepted instead of failing with "unsupported Hermes plugins config" — an empty `enabled: []` is rewritten to a block list. The generated context engine additionally honors a `project_root` config key so profiles can pin the indexed project (explicit host kwargs win; the session cwd stays the last fallback).
- **`tracedecay install --agent hermes --project-root <abs path>` pins a profile's plugin to one project.** The pin is written into the generated plugin (`PINNED_PROJECT_ROOT` in `tools.py`): every plugin tool call then passes `--project <pin>` so memory + LCM stores resolve to `<pin>/.tracedecay/` regardless of the Hermes process cwd, and the context engine uses it ahead of cwd inference (kwargs > config > pin > cwd). Reinstalls without the flag preserve an existing pin; the flag is hermes-only, requires an absolute path, and conflicts with `--all-profiles` (pins are per-profile).
- **`tracedecay tool` now walks up from subdirectories to the nearest initialised project** when `--project` is not given, matching how `sync`, `status`, `serve`, and `dashboard` resolve project roots.
- **Cursor hook hints use the quote-aware shell parser.** `tool_hints` classified search commands with a naive `split_whitespace`, so a quoted pattern like `grep "needle -r" file` leaked a fake `-r` flag and misclassified as a recursive search. It now shares `hooks.rs`'s quote/escape-aware `shell_words` parser (single shared implementation, regression-tested).
- **Hermes generated plugin files are written atomically.** `write_text_file` now uses the write-to-`.new`-then-rename pattern (like the config writer), so a mid-write crash can no longer leave a truncated `__init__.py`/`tools.py` behind. Unsupported-config errors during install also name the exact retry command (`tracedecay install --agent hermes`).
- **v13 archive-column cleanup handles generated-column dev databases.** The migration enumerated columns with `PRAGMA table_info`, which hides GENERATED columns — so a dev DB where the abandoned archive revision left `superseded_by` as a generated column referencing `merged_into` skipped that drop and then failed with `no such column: merged_into`. The migration now uses `PRAGMA table_xinfo` and drops the columns in reverse-addition order (dependent generated columns first). Covered by a regression test seeding exactly that odd state.
- **Archive-semantics purge (policy: deleted memories are permanently hard-deleted).** Removed the last UI remnants of the never-shipped archive feature: the CurationPanel no longer recognizes/renders `archive` or `supersede` ops (neither planner can produce them; the curate ops contract is delete/merge only), the `archive` action field is gone from the frontend types, and stale Hermes-wrapper docstrings naming `archive`/`archive/{fact_id}/restore` routes were corrected. A new store-level test pins the full hard-delete cascade: `MemoryStore::remove_fact` removes the fact row, its FTS mirror row, its entity links, and its feedback events, and marks the fact's banks dirty.

## [6.1.3] - 2026-06-04

### Fixed
- **Write/exec MCP tools no longer advertise `readOnlyHint: true` (#94).** `tracedecay_replace_symbol`, `tracedecay_insert_at_symbol`, and `tracedecay_run_affected_tests` mutate source files or run a `cargo test` subprocess, but were annotated read-only via the shared `def()` helper — so harnesses that auto-approve read-only tools could edit files or compile and execute project code without prompting. They now use a new `def_rw()` helper that stamps `readOnlyHint: false`, matching the other edit tools. A regression test asserts every write/exec tool is non-read-only.

## [6.1.2] - 2026-05-30

### Added
- **Svelte extractor (`.svelte`).** Locates all `<script>` blocks (instance and `<script module>` / `<script context="module">`), blanks out template markup, and delegates to the TypeScript extractor. Symbols, interfaces, exported functions, and `$props`/`$derived` rune declarations are indexed with correct line numbers in the original file. Closes #93.
- **Astro extractor (`.astro`).** Parses the `---` TypeScript frontmatter block at the top of the file. Imports, interfaces, exported functions, and top-level `const`/`let` declarations are indexed; the HTML template body below the closing `---` is ignored. Closes #93.

### Fixed
- use OpenCode correct global rules file path (`AGENTS.md` instead of `OPENCODE.md`) (thanks @davidefossacecchi, #92)

## [6.1.1] - 2026-05-26

### Added
- **Borrowed-worktree detection on every MCP read tool.** When a git worktree is nested inside the main checkout (e.g. agent tooling that puts worktrees under `.claude/worktrees/<name>/` or `.worktrees/<name>/`), tracedecay's `discover_project_root` walks up and silently resolves the MAIN checkout's `.tracedecay/` — returning results for the wrong branch with no warning, while symbols changed only in the worktree are invisible. New `src/worktree.rs` runs `git rev-parse --show-toplevel` on the caller's CWD and on the resolved index root; when they belong to different working trees, the verbose warning is included in `tracedecay_status` and a one-line notice is prefixed to every read tool response. Detection runs once at server startup (≤2 `git rev-parse` spawns total per session). Ported from codegraph #312.
- **Catch-up sync on MCP connect.** `McpServer::new` now spawns a non-blocking task that runs `find_stale_files` + `sync_if_stale_silent` + `refresh_file_token_map` once at startup, bypassing the 30 s cooldown. Picks up changes made while the server was down — terminal `git pull`, IDE edits before the agent launched, files touched by another tool — so the first tool call sees a fresh index instead of waiting through the cooldown. Ported from codegraph #414.
- **`scripts/prepare-release.py` to auto-promote `[Unreleased]` → `[<version>]` in CHANGELOG.md.** Idempotently renames the `[Unreleased]` block to a dated `[<version>]` block at release time (Case A), or merges into a pre-existing `[<version>]` block by sub-section (Case B). Avoids the codegraph v0.9.5 failure mode where a sparse hand-staged `[<version>]` block silently shadowed the much-larger `[Unreleased]` section above it during release-notes extraction. Ported from codegraph #436. Wire into the release workflow when a version bump lands.

### Changed
- **Embedded MCP watcher replaced with on-demand staleness check (#80).** The `notify-debouncer-full` watcher was the source of severe CPU and memory pressure on large monorepos: top-level-only filtering of `IGNORED_DIRS` meant nested `apps/*/node_modules`, `packages/*/target`, `**/dist` were watched at the OS level, producing event storms and unbounded `RecommendedCache` growth (one user reported the process climbing to 19 GB before being killed). The watcher is now gone — along with the `notify-debouncer-full` dependency. Index freshness is maintained by a lazy `find_stale_files` walk (same gitignore-aware logic `sync()` uses) gated by a 30-second cooldown and invoked at the top of every MCP `tools/call`. Cost: walks on the cold tool call after a quiet window add tens of ms to milliseconds depending on repo size; in exchange, the unbounded-memory class of bug is structurally gone. Reported by @AGiorgetti and @ottob.
- **Per-file staleness banner replaces the binary "STALE INDEX" warning.** Tool responses that referenced files whose in-line sync couldn't refresh now get a focused banner naming exactly those files with their edit ages (e.g. `src/foo.rs (edited 3m ago)`) and an explicit instruction to `Read` those files directly — while telling the agent the rest of the response is authoritative. Replaces the previous all-or-nothing wording that made agents distrust the entire response. The machine-readable `tracedecay_graph_stale` marker is preserved. Ported from codegraph #428.
- **Kiro steering is loaded as a resource.** The Kiro installer now writes `~/.kiro/steering/tracedecay.md`, loads it from the managed agent's `resources` list with an absolute `file://` URI, leaves the custom-agent `prompt` unset so Kiro's default prompt is preserved, keeps MCP approval policy out of `mcp.json`, and installs permissive `tools: ["*"]` plus `allowedTools: ["@builtin", "@tracedecay"]` defaults for the managed agent.

### Fixed
- **Windows: same physical file no longer indexed twice under both path-separator variants (#87).** On Windows, sync entry points that accepted caller-supplied relative paths (`sync_if_stale`, `sync_if_stale_silent`, `sync_single_files`, `check_file_staleness`) did not normalize backslashes before hitting the DB. When an MCP tool response carried OS-native paths (`src\foo.py`) into the staleness check, the lookup missed the canonical row stored under `src/foo.py`, the file got re-indexed under the backslash variant, and the `files` table grew a duplicate row for every physical source file. Downstream tools (`god_class`, `complexity`, `dead_code`, `redundancy`, `unused_imports`) returned doubled results; the `health` `redundancy` dimension reported ~0.39 instead of ~0.50+. Fix: defensive `normalize_rel_path` (single `replace('\\', "/")`) applied at all four sync entry points, with regression tests. Reported by @xaerogonzo.

## [6.1.0] - 2026-05-25

### Added
- **`tracedecay tool <name>` — schema-driven CLI dispatcher.** Every MCP tool is now reachable from the command line through a single dynamic subcommand that introspects each tool's JSON schema and coerces `--key value` flags accordingly. `tracedecay tool` (no args) lists tools grouped by category; `tracedecay tool <name> --help` prints schema-derived parameters. Reserved flags: `--json` (raw response), `--project <path>`, `--args <json>`, `-h`/`--help`. Positional args bind to required string properties (e.g. `tracedecay tool search foo`). `@file` values are read from disk for multi-line strings. Replaces seven hand-rolled subcommands (`query`, `context`, `body`, `impact`, `callers`, `files`, `affected`); `query` is kept as an alias for the renamed `search`. New file: `src/tool_command.rs` (~729 LoC).
- **`tracedecay_find_exact_symbol` MCP tool.** Bare-name lookup against `idx_nodes_name` — a single O(log n) index probe with no BM25 ranking, no fuzzy match, no qualified-name suffix walk. Use this when the symbol name is already known; use `tracedecay_search` for relevance-ranked discovery. ~30–200 µs per lookup vs. ~700 µs for the BM25 path.
- **`tracedecay_call_chain` MCP tool.** Finds the shortest *directed* call chain between two node IDs along outgoing `calls` edges only. New `find_path_directed` BFS in `graph/traversal.rs` (the existing `find_path` is bidirectional and wrong for "how does A reach B" questions). Bounded by `max_depth` (default 8, max 20).
- **`tracedecay_file_dependents` MCP tool.** Lists every indexed file that imports or otherwise depends on the given file. Thin wrapper around the existing `TraceDecay::get_file_dependents` that was previously only reachable through `tracedecay_affected`'s test-rollup path.
- **`tracedecay_replace_symbol` MCP tool.** Symbol-aware body replacement: resolves a name via exact qualified-name match, narrows to callable kinds on ambiguity, and refuses the edit rather than picking the wrong site if more than one callable matches. Reads the file, splices the symbol's `start_line..=end_line` range with `new_source`, writes back, and reindexes the touched file. Plays the role of token-savior's `replace_symbol_source`.
- **`tracedecay_insert_at_symbol` MCP tool.** Inserts content immediately before or after a named symbol's source range — same resolution semantics as `tracedecay_replace_symbol`. `position` is `"before"` or `"after"` (default after). Plays the role of token-savior's `insert_near_symbol`.
- **`tracedecay serve --timings` flag.** When set, every `tools/call` response gains a `_meta.duration_us` field reporting the handler's pure execution time in microseconds. Lets clients (and benchmarks) attribute latency to actual query work vs. JSON-RPC / stdio / Python-parse overhead. Toggleable at runtime through the new `McpServer::set_timings_enabled` setter so embedders can flip it per-session.
- **Indexer benchmark harness** at `benchmarks/run_benchmarks.py` — adapts `Mibayy/token-savior`'s `run_benchmarks.py` to drive both tools side-by-side on the same clone of FastAPI, sharing a random symbol sample (seed=42) so per-query rows are directly comparable. tracedecay is driven through a long-lived `tracedecay serve --timings` MCP session for the query column. Latest report (`benchmarks/comparison-report.md`): cold index 2.9× faster, impact analysis 43× faster than token-savior.
- **tsbench fork** at `benchmarks/tsbench/` — patch + reproduction README + per-run summary for running `Mibayy/tsbench` (token-savior's own 96-task agent benchmark) against tracedecay. First-attempt untuned result: 184/192 = 95.8% vs. token-savior's audited 97.9%. The harness rewrites `SYSTEM_PROMPT_TS` to map each token-savior tool to its tracedecay equivalent and relaxes the `--disallowedTools` list to allow `Read`/`Edit` fallback on the four task categories tracedecay has no direct tool for.
- **`docs/TRACEDECAY-VS-TOKENSAVIOR.md`** — full capability + performance comparison document covering parsing strategy (regex annotators vs. tree-sitter grammars), the 11 health-analytics tools that have no token-savior equivalent, query-latency numbers (apples-to-apples find / body / impact), the tsbench 184/192 result with per-task failure analysis, and an honest "when to use which" guide.

### Fixed
- **`tracedecay serve` no longer blocks MCP `initialize` on the watcher's filesystem walk (#84).** Constructing the embedded `notify_debouncer_full` watcher does a synchronous `walkdir` over every registered subtree to seed its file-id map. On a large JS/TS monorepo with multi-gigabyte `node_modules` / `.next` / `dist` trees this can take 30+ seconds — long enough to blow the client's `initialize` timeout. Fix: `ProjectWatcher::new` now runs inside `tokio::task::spawn_blocking` from a detached `tokio::spawn`, so `McpServer::new` returns immediately and the MCP stdio loop can answer `initialize` / `tools/list` in milliseconds. The `CancellationToken` is stored on the server up front so `shutdown` can cancel mid-walk if the agent disconnects before the watcher finishes initialising. Reported by @ottob with a sample-trace and an FSEvents-sandbox repro that left zero ambiguity about root cause.
- **`tracedecay serve` no longer runs pre-serve maintenance work (#84).** `Commands::Serve` was running `try_flush` (synchronous HTTP round-trip to the worldwide counter), `check_install_stale`, and the silent-reinstall loop over every tracked agent before the MCP stdio loop even started. All three are now gated behind `should_skip_agent_install_maintenance`, alongside `Install` / `Reinstall` / `Uninstall` / `Doctor`. Same maintenance still runs on the user's next interactive `tracedecay …` invocation.
- **`tracedecay install --agent antigravity` now registers in both the IDE config and the CLI plugin directory (#85).** Previously only `~/.gemini/antigravity/mcp_config.json` was written, leaving the Antigravity CLI (`agy`) unable to see tracedecay in `/mcp`. New: also writes `~/.gemini/antigravity-cli/plugins/tracedecay.json` with the same `{"mcpServers": {"tracedecay": {...}}}` shape. `uninstall` removes both, `doctor` reports both, `is_detected` triggers on either path. Reported by @ottob.
- **MCP `last synced N ago` warning no longer fires after a no-change sync (#86).** The warning was reading `MAX(files.indexed_at)`, which only advances when a file is actually reindexed. On quiet repos a successful `tracedecay sync` (0 added / 0 modified / 0 removed) left `indexed_at` stuck and the warning fired forever. New: the warning is computed from the `last_sync_at` metadata key, which `sync()` writes unconditionally on every successful invocation. Falls back to `MAX(indexed_at)` only when the metadata key is missing (e.g. a freshly-initialised project that has never been synced). New `TraceDecay::last_sync_timestamp()` helper exposes this for embedders. Reported by @uwe-sure.
- **`files_by_language` status output now uses real language names instead of bucketing everything as `Other`.** The SQL `CASE` in `Database::get_stats` only recognised four languages (Rust / Go / Java / Scala) and dumped everything else — Python, TypeScript, C, Swift, Kotlin, etc. — into `"Other"`. Replaced with a Rust-side bucketing helper covering 46 extractor languages; Python files in the FastAPI benchmark now correctly report as `Python` instead of `Other`. Includes special-case basename matching for extensionless `Dockerfile` / `Makefile`.
- **Pre-existing breakage in `tests/mcp_server_test.rs` repaired.** The whole `test-transport`-gated integration suite (31 tests) had been silently failing to compile since `McpServer::new` switched its return type to `Arc<McpServer>` — `setup_server` and `run_server_with_messages` still expected bare `McpServer`. Switched both helpers to `Arc<McpServer>` and bumped the resource-count assertion (`tracedecay://status/files/overview/branches`) from 4 to 5 to include the newer `tracedecay://schema` resource. All 31 tests now pass.

### Changed
- **Seven hand-rolled CLI subcommands replaced by the unified `tracedecay tool <name>` dispatcher.** `query`, `context`, `body`, `impact`, `callers`, `files`, `affected` were each ~50–150 LoC of clap glue duplicating what the MCP tool already declares in its schema. All seven are gone; the same operations are reached as `tracedecay tool query …`, `tracedecay tool body …`, etc. Drops ~600 LoC of dispatch boilerplate from `src/main.rs`. `query` is kept as an alias for the renamed `search` so muscle memory still works.

### Internal
- New methods on `TraceDecay`: `get_call_chain`, `get_nodes_by_name`, `replace_symbol`, `insert_at_symbol`, plus a private `resolve_symbol_for_edit` helper that does qname-first resolution and refuses ambiguous matches rather than silently picking the wrong site.
- New method on `GraphTraverser`: `find_path_directed` (BFS that follows outgoing edges only, with `max_depth` bound).
- `McpServer` now has a `timings_enabled: AtomicBool` field with `set_timings_enabled` / `timings_enabled` accessors, toggleable per-session.
- The dispatcher in `mcp/tools/handlers/mod.rs` wraps each `handle_tool_call` invocation with `Instant::now()` when `timings_enabled` is set and injects `_meta.duration_us` into the JSON-RPC result.

## [6.0.0] - 2026-05-25

### Breaking
- **Daemon mode removed.** The `tracedecay daemon` subcommand, autostart flags (`--enable-autostart` / `--disable-autostart`), foreground mode, and all `daemon-kit`-backed service registration are gone. ~1,100 lines of platform glue (launchd plists, systemd user units, Windows SCM, PID files, UAC elevation) deleted. File-watching now lives inside the MCP server itself — it spawns a `notify`-backed watcher that runs `sync_if_stale_silent` for the duration of the agent session, and exits when the agent disconnects. Multiple MCP peers on the same project converge through the existing per-project sync lock plus `sync_if_stale_silent`'s peer-coordination check; no new primitive was needed. Users with a registered autostart service should unload it manually — see "Migration" in `docs/TRACEDECAY-WHATSNEW.md` §6.0.0.
- **`UserConfig::daemon_debounce` renamed to `watcher_debounce`.** TOML load is backwards-compatible via `#[serde(alias = "daemon_debounce")]` and any config-mutating command rewrites the file with the new name; Rust struct literals referencing the old name are a compile-time break.
- **`McpServer::new` now returns `Arc<Self>`.** The embedded watcher task captures a `Weak<Self>` so it cannot extend the server's lifetime. Embedders that bound the return value continue to compile; destructuring by value or storing into a non-`Arc` field needs to adapt.
- **`tracedecay install --agent claude` writes the modern hook shape `{type, command, args}`.** Legacy single-string `"command": "<bin> <subcmd>"` entries are detected by `tracedecay doctor` and auto-rewritten using `current_exe()` as the binary path (issue #81). This is a breaking change for any external tooling that introspects `~/.claude/settings.json` and assumed the legacy concatenated form.
- **Beta release channel disabled.** `.github/workflows/release-beta.yml` is gated behind `BETA_CHANNEL_ENABLED=false` and a `workflow_dispatch`-only trigger. The code is preserved for future revival; no `*-beta.*` versions will ship from this commit forward.

### Added
- **`tracedecay_redundancy` MCP tool (#83).** AST-level functional-duplicate detector. Computes four signals per function/method body via tree-sitter — AST shape hash, control-flow-graph hash, ordered call-sequence hash, and a 5-gram token-shingle set — then blends them into a `[0, 1]` composite similarity score (weights 0.40 / 0.25 / 0.20 / 0.15). Pairs are bucketed `definite` / `likely` / `naming_only` and ranked by score. Language-agnostic by design: kind walks use raw tree-sitter strings, so the same code path works for every supported grammar. Computation is lazy — fingerprints land in a new `node_fingerprints` table (schema v10) keyed by `(node_id, body source hash)` and persist across MCP sessions. Pairwise comparison is bucketed by body-token count (±25 % window) so it stays sub-quadratic on large repos.
- **`tracedecay_runtime` MCP tool + `tracedecay status --runtime` flag (#80).** Captures a process + database telemetry snapshot: PID, RSS, virtual size, sustained CPU% sampled over 200 ms, uptime, host CPU count, total system memory, DB / WAL / SHM file sizes, `journal_mode` PRAGMA, total indexed source bytes, node and edge counts, and a derived `db / source` bloat ratio. Lets users hitting unexpected resource pressure attach a structured snapshot to a bug report. Text report mirrors the `tracedecay status` layout; JSON output via `--json` for machine consumption.
- **`tracedecay_health` `details=true` sub-score breakdown (#82).** Returns per-dimension `{ score, interpretation, raw_count, source }` objects covering acyclicity (with `edges_in_cycles`), depth (`max_chain` / `ideal_chain`), equality (gini + textual interpretation), redundancy (`dead_count` / `total_fns`), modularity (textual label + components-after-hub-removal), and coverage discipline (`skip_test_coverage_count`). The composite `quality_signal` (geometric mean × 10 000) is preserved as the headline figure.

### Changed
- **File-watcher rewritten around `notify-debouncer-full` 0.8.0-rc.2.** Replaces the DIY tokio debounce timer with the maintained library, which coalesces rename pairs, suppresses redundant modify-after-create, and batches event bursts cross-platform. Drop-in transparent to callers — `ProjectWatcher::new(root, debounce)` signature unchanged.
- **Watcher no longer recursive-watches the project root.** Top-level entries are enumerated at startup; `IGNORED_DIRS` (`target/`, `node_modules/`, `.git/`, …) and dotdirs (`.vscode`, `.idea`, …) are *never registered as watches*, so the kernel never reports events for them. The root itself is watched non-recursively to surface new top-level directories appearing after startup. This is the primary mitigation for the high CPU/RAM symptoms reported in #80 on Windows, where `ReadDirectoryChangesW`'s per-watch buffer could be overwhelmed by churn inside a large `node_modules`.
- **Direct `notify` dependency dropped.** Pulled transitively through `notify-debouncer-full`; project_watcher imports types via `notify_debouncer_full::notify` to keep a single crate instance in the resolver graph (debouncer 0.8.0-rc.2 depends on `notify` 9.0.0-rc.4).
- **`doctor` auto-repair logic for hook entries.** Modern-shape hooks with the wrong subcommand are fixed in place (preserving the user's bin path); legacy single-string hooks are rewritten using `current_exe()` since the embedded path cannot be parsed unambiguously when it contains spaces. Breaks the doctor → install loop that issue #81 reported on Windows path-with-spaces installs.

### Fixed
- **Hook install on paths with spaces (#81).** Previously, `install --agent claude` wrote `"command": "C:/Path With Spaces/tracedecay.exe hook-pre-tool-use"` as a single string. Claude Code whitespace-splits the field, so the kernel executed `C:/Path` with arg `With` and the hook silently never fired. `doctor` correctly diagnosed the mismatch but then re-installed the same broken shape, looping forever. Switching to `{"command": "...", "args": ["hook-..."]}` sidesteps the splitter entirely. Doctor now also recognises the legacy shape as needing repair, not removal.

### Schema
- **Migration v10.** Adds the `node_fingerprints` table backing `tracedecay_redundancy`: `node_id PRIMARY KEY`, `ast_hash`, `cfg_hash`, `call_seq_hash`, `shingles` (comma-separated lowercase hex), `body_tokens`, `source_hash`. Indexed on `ast_hash` and `body_tokens` for the redundancy query's bucketed scan. As with prior migrations, opening a v9 database triggers a one-time full re-index on first open.

### Internal
- **New `src/redundancy.rs` module.** Tree-sitter walk utilities (kind-only AST hash, control-flow filtered hash, ordered call-sequence hash), token shingle generator, Jaccard similarity over sorted shingle sets, composite similarity blender, and the severity bucketing rules. Eight unit tests cover identical-body / different-structure / call-sequence-order / Jaccard self-similarity behaviour.
- **New `src/runtime_telemetry.rs` module.** `RuntimeSnapshot { process, database }` collected via `sysinfo` 0.32 + libsql PRAGMA reads. Process side requires a refresh + 200 ms sleep + refresh sequence because sysinfo reports CPU% as a delta between successive readings.
- **`TraceDecay::db_path()` accessor.** Recomputes the on-disk DB path via `resolve_db_for_branch` for diagnostics. Stable across the lifetime of an open `TraceDecay`.
- **Tool registry grew from 68 → 71 (or 67 → 70 when `ast-grep` is unavailable).** Two new entries: `tracedecay_redundancy`, `tracedecay_runtime`. The previously shipped `tracedecay_health` `details=true` schema knob is documented here for visibility.

## [5.1.2] - 2026-05-20

### Added
- **Kiro agent integration.** `tracedecay install --agent kiro` now installs the MCP server, global steering, managed Kiro agent config, default CLI agent selection, and Kiro hook mappings for prompt context, delegated tool context, and post-write re-indexing. Doctor and uninstall support are included with coverage for workspace overrides and idempotent cleanup.

### Fixed
- **Edit tool UTF-8 failure handling (#78).** `tracedecay_multi_str_replace` and `tracedecay_insert_at` no longer panic when failure previews or long anchors contain multi-byte UTF-8 characters.
- **GW-BASIC / MSBASIC2 REM stripping.** Comment extraction no longer relies on a byte-length guard; the `REM` keyword is now content-checked (case-insensitive) before slicing, eliminating a latent panic on multi-byte comment text.

### Changed
- **Shared `text::utf8_prefix_at_or_before` helper.** The UTF-8-safe byte-budgeted prefix logic introduced for the edit-tool fix is now a shared `crate::text` utility, and `context::builder` reuses it in place of its hand-rolled `is_char_boundary` walk-back.

## [5.1.1] - 2026-05-16

### Performance
- **`tracedecay_dead_code` no longer times out on chromium-scale repos.** The pre-4.14.8 form ran the leading-wildcard `LIKE '%::test'` chain inside a correlated `NOT EXISTS` on every dead-code candidate row — fast on scirs (0.097 s, 76 K `annotation_usage`) but timed out at the 25 s probe ceiling on chromium, cascade-poisoning every subsequent MCP tool call via JSON-RPC id reuse. 4.14.8's `WITH test_marker_ids AS (...)` CTE attempt regressed scirs from 0.1 s to >60 s because SQLite inlined the single-reference CTE, so the wildcard scan ran per candidate row instead of once; that attempt was reverted in 4.14.9. A first attempt that put marker ids into a single TEMP table and probed via `e2.source IN (SELECT id FROM temp.test_markers)` ALSO failed on chromium: SQLite picked `idx_edges_unique (source, target, kind)` for the correlated subquery and iterated every marker as the outer driver for every candidate (~13 K markers × ~134 K candidates ≈ 1.7 B probes), >60 s. New shape — **three-step resolve + pre-join + probe via TWO TEMP tables**:
  - `Database::collect_test_marker_ids` runs the marker `SELECT` exactly once over the `kind = 'annotation_usage'` partition (indexed via `idx_nodes_kind`).
  - `Database::populate_test_marker_temp_table` drops + recreates `temp.test_markers` (with `PRIMARY KEY` on `id` so SQLite builds a real B-tree) and bulk-inserts in 500-id chunks.
  - `Database::populate_test_annotated_targets_temp_table` joins `edges WHERE kind = 'annotates' AND source IN temp.test_markers` once, materialising "which node ids are annotated by any test marker" into `temp.test_annotated_targets` (PK on `target`). ~15 K rows on chromium.
  - `find_dead_code`'s outer SELECT then uses `nodes.id NOT IN (SELECT target FROM temp.test_annotated_targets)` — a single PK probe per candidate against a small indexed lookup table, the optimiser cannot re-shape this into a per-marker iteration.
  - Both temp tables are unconditionally dropped on the wrap path so a failed query does not leak rows to the next caller on the same connection.

  Inline comment block on `find_dead_code` documents all three prior pathologies (pre-4.14.8, 4.14.8 CTE, single-temp-table attempt) and a `DO NOT regress this` warning to forestall the next attempt.

  Verified end-to-end via the MCP probe (`scripts/mcp_probe/probe.py`) against the real chromium DB (7.5 GB, 4.4 M nodes, 206 K `annotation_usage`, 411 K annotates edges):

  | call | duration | notes |
  |---|---|---|
  | `dead_code {}` | 2.45 s | cold cache (first call after server start) |
  | `dead_code {limit: 10}` | 1.10 s | warm |
  | `dead_code {include_public: false}` | 1.10 s | warm |
  | `dead_code {path: "src"}` | 1.10 s | warm |
  | `dead_code {path: "lib"}` | 1.09 s | warm |

  Was 5/5 TIMEOUT @ 25 s pre-fix, cascade-poisoning every subsequent tool in the probe matrix. Direct `sqlite3` runtime measurement (no MCP layer): 0.75 s end-to-end (markers 42 ms → targets 102 ms → main 600 ms). On scirs (76 K `annotation_usage`): 0.6 s end-to-end — a regression-acceptable trade-off vs. the 0.097 s pre-4.14.8 baseline given that chromium went from >25 s timeout to <1.1 s steady-state. The synthetic regression test `tests/graph_test.rs::dead_code_marker_resolve_is_single_pass` (50 K `annotation_usage` / 5 K functions) runs in 1.3 s release / 3.6 s debug with a 5 s assertion ceiling.

## [5.0.0] - 2026-05-16

The largest functional jump since 4.0: nine new MCP tools, a cross-session response cache, and a schema-level rework of containment.

### Added

- **`tracedecay_read`** — mode-aware file read (`full`, `lines`, `map`, `signatures`) with cross-session cache. `map` and `signatures` are graph-only — no source bytes are touched. A re-call on an unchanged file returns a ~30-token `{"unchanged": true, …}` stub. The cache key folds `last_sync_at` for graph-backed modes so a force-reindex correctly invalidates derived rows.
- **`tracedecay_outline`** — flat list of every top-level symbol in a file, with optional kind filter. The cheapest way to orient before zooming into a large file.
- **`tracedecay_implementations`** — find every type implementing a given trait, or every body of a given method name. Returns method bodies with signatures.
- **`tracedecay_unsafe_patterns`** — surface `.unwrap()` / `.expect()` / `panic!` / `todo!` / `unimplemented!` / `unsafe { }` sites with an `in_test` flag. Word-boundary matching avoids `.unwrap_or` false positives; an `exclude_tests` option skips test-shaped paths.
- **`tracedecay_diagnostics`** — runs the project's compile / type checker (cargo / tsc / pyright) and returns structured errors mapped to graph nodes. Replaces the recurring "shell out → parse text → read file" loop with one structured response. Cargo target dir is forced to `.tracedecay/target/` so it can't race with the user's interactive cargo runs.
- **`tracedecay_config`** — query TOML / JSON config files by dotted key path. Single file (`path`) or glob (`glob`); returns parsed value plus a heuristic line number. DB-free — works on uninitialized projects.
- **`tracedecay_signature_search`** — find functions / methods by signature shape: return type, parameter substring, async flag, path filter. All filters AND-compose.
- **`tracedecay_constructors`** — locate every literal-instantiation site of a struct (`Foo { … }`) and report which fields each site sets — plus `missing_fields` relative to the struct's current definition. The classic "I added a required field, what breaks?" question. String- / char-literal awareness and `match` / `if let` / `while let` pattern filtering keep the result list clean.
- **`tracedecay_field_sites`** — partition every `.<field>` reference into reads and writes. Writes include `=`, compound assignments, and `&mut x.field` borrows; `==` and `=>` correctly count as reads.
- **`tracedecay bench` colored console output** — default `tracedecay bench` is now a fixed-width colored table instead of a markdown dump. Compact `k` / `M` numeric units; savings percentages colored by tier (green ≥80 %, yellow ≥50 %, red <50 %); aggregate footer in the same tier color. `--json` is unchanged.

### Changed

- **Schema v9: cross-session response cache.** New `read_cache` table keyed by `(project_id, session_id, file_path, mode, args_hash)` with `mtime_ns` for freshness. Backs `tracedecay_read`.
- **Schema v9: `Contains` edges denormalized into `nodes.parent_id`.** The same migration folds containment off the edges table and onto a new column. Cleaner queries — `get_children_of(parent_id)` is one indexed lookup — and the read-only SQL layer no longer has to filter by edge kind for every "find members of this container" question. Extractors keep emitting `Contains` edges as before; the storage layer hoists them into `parent_id` at insert time and skips persisting the row.

### Migration notes

- **v9 is forward-only.** First sync after upgrade auto-applies the migration, populates `parent_id` from existing `Contains` rows, and deletes those rows.
- **Recovery path: `tracedecay sync -f`.** If a downstream consumer still queries `Contains` edges directly (none of the in-repo tools do), force-sync rebuilds the graph from source under the new schema.
- External SQLite consumers reading the `edges` table should switch from `kind='contains'` filters to `nodes.parent_id` joins.

## [4.14.11] - 2026-05-16

### Performance
- **Same `node.child(i)` O(N²) trap fixed in `batch_extractor.rs`.** `visit_top_level`, `visit_label`, `extract_docstring`, and `extract_label_call_sites` all walked top-level children of the Batch program via `root.child(i)` in an index loop. Refactored: `visit_top_level` materialises children once into a `Vec<TsNode>` via cursor (`collect_children` helper) and downstream helpers take `&[TsNode]` + index instead of `(root, index)`. One O(N) allocation up front, O(1) lookups thereafter, no behavior change.
- **Same trap fixed in `powershell_extractor.rs::find_descendant_by_kind`.** The iterative DFS pushed children with `current.child(i)` in a `for i in (0..N).rev()` loop. Replaced with a `TreeCursor` walk + `stack[start..].reverse()` to preserve first-child-pops-first order — matches the `complexity.rs::push_children` pattern from 4.14.10.
- **Same trap fixed in `clojure_extractor.rs::extract_calls`.** Top-level form iteration over `list_lit` children used `node.child(i)` indexed loop. Replaced with cursor stepping (with `goto_next_sibling` `skip` times for the `skip` parameter). Particularly relevant on Clojure files with hundreds of top-level forms.
- **Same trap fixed in `cobol_extractor.rs::visit_procedure_division`.** The seed pass that collects PROCEDURE DIVISION children into a `Vec` for multi-pass paragraph grouping used `node.child(i)` in a loop — bites on monolithic COBOL files with many paragraphs. Switched to cursor walk; same O(N) materialisation, O(1) downstream indexing.

## [4.14.10] - 2026-05-16

### Performance
- **`count_complexity` (called from every extractor on every function) no longer hits an O(N²) trap on high-fanout AST nodes.** The body-walk in `src/extraction/complexity.rs` seeded its stack and pushed children with `node.child(i)` inside a `for i in 0..N` loop. Tree-sitter's `node.child(i)` is **O(i)** — it walks the linked sibling chain from the first child — so the seed + per-pop push pair was O(N²) for every node along the way. On `kernel/bpf/verifier.c` (20 K lines, monster switch statements with thousands of cases) a single `tracedecay init` showed the progress bar wedged on that one file long enough that users reported it as "stuck"; chromium had files taking ~3 min individually. New `push_children` helper uses a `TreeCursor` (O(1) per sibling step) and reverses the appended slice so LIFO pop order still produces left-to-right traversal. Same fix applied to `extract_call_name`, `extract_macro_name`, and `rightmost_identifier` — all three did the same O(N²) `child(i)` scan over identifier candidates. Measured on `verifier.c` after the fix: 78 ms end-to-end (file read + parse + extract). Includes `examples/bench_extract.rs` so you can re-measure with `cargo run --release --example bench_extract <path-to-c-file>`.

## [4.14.9] - 2026-05-16

### Fixed
- **Revert the 4.14.8 `find_dead_code` CTE refactor — it was a massive regression on real repos.** 4.14.8 moved the test-marker name match into `WITH test_marker_ids AS (...)` thinking that would amortise the leading-wildcard `LIKE`. In practice SQLite does not always materialise a single-reference CTE, and `e2.source IN (SELECT id FROM test_marker_ids)` inside a correlated `NOT EXISTS` degenerated into a per-row scan of the full `annotation_usage` table. On scirs (76 K annotation_usage rows, 153 K annotates edges) `tracedecay_dead_code` went from **0.097 s** (pre-4.14.8) to **>60 s timeout**, which hung the MCP probe matrix — every subsequent tool then appeared to time out because the late response poisoned the JSON-RPC id matching (the cascade caveat in `scripts/mcp_probe/README.md`). The original `JOIN nodes a ON a.id = e2.source` form works because `idx_edges_target_kind` narrows to the (typically 0-3) annotates edges per candidate first, then joins via the nodes PK, so the LIKE only runs on that small per-candidate slice. A `Do NOT lift this into a CTE` comment is left at the call site so future refactors don't repeat the mistake. Other 4.14.8 perf changes (SCC frame clone, multi-source BFS, lines cache, dedup'd FTS terms, file-content cache, cycle-path borrowing, inheritance-depth CTE shape, has_bare_call fast path, placeholder builder) are kept as-is.

## [4.14.8] - 2026-05-16

### Performance
- **Tarjan SCC no longer clones the entire neighbor list per edge visited.** The iterative DFS in `graph/scc.rs` used `work.last_mut().cloned()` on each loop iteration — that deep-copies the top tuple `(node, neighbors, idx)` including the whole `Vec<N>` of neighbors, once per neighbor visited (so ~`out-degree × visits` full Vec clones during a node's life). Rewrote the loop to peek the top frame with `work.last_mut()` and clone only the two values actually needed (`node` and `next`) before any `work.push(...)`. Every SCC consumer benefits: `tracedecay_circular`, `tracedecay_port_order`, and (since 4.14.7) `tracedecay_recursion`.
- **`tracedecay_diff_context` walks the impact radius once for the union of modified symbols, not once per symbol.** The old loop called `get_impact_radius(node.id, depth)` for every modified node — each call ran an independent BFS from scratch, so any downstream node reachable from K modified ancestors got re-traversed K times. New `GraphTraverser::get_impact_radius_multi(seed_ids, max_depth)` does one BFS seeded with all modified node IDs and a single shared `visited` set; the result has every reachable node visited at most once. Surfaces as `TraceDecay::get_impact_radius_multi`. Particularly impactful on diamond-dependency hotspots (shared utility files reachable from every changed module).
- **`tracedecay_recursion` caches source-file lines instead of re-splitting on every self-edge check.** `is_direct_self_call` was caching the raw `String` source but then doing `let lines: Vec<&str> = source.lines().collect();` on each call — for a 10 k-line file with N self-edges, that's N × 10 k allocations purely to throw away. Cache changed to `HashMap<String, Option<Vec<String>>>` so the line vector is built once per file.
- **`tracedecay_recursion` cycle-path DFS uses borrowed `&str` and stops once the limit is hit.** `cycle_path_for_scc` / `dfs_cycle_path` previously used `Vec<String>` / `HashSet<String>` on hot paths, allocating a `String` per neighbor visit even though every id already lived in `scc_set`. Switched to `&str` borrows over the SCC's existing storage. The outer loop also sorts SCCs by length first and short-circuits as soon as `cycles.len() == limit`, so we no longer enumerate every cycle in a giant mutually-recursive graph before truncating.
- **`tracedecay_inheritance_depth` CTE collapses the hierarchy before joining `nodes`.** The recursive CTE produced one row per (leaf, depth) pair across the full hierarchy; the outer SELECT then ran the `file_path LIKE ?` filter over all of them. Wrapped the hierarchy in a `leaf_depths` CTE that `GROUP BY leaf_id` first, so the path filter and node join only see distinct leaves.
- **`tracedecay_dead_code` resolves the test-marker annotation set in a single CTE pass.** Each candidate dead-code row previously re-evaluated `a.name LIKE '%::test'` (and three more leading-wildcard `LIKE`s) — none of those can use an index, so the cost scaled with `dead-candidates × annotation_usage`. New `WITH test_marker_ids AS (…)` resolves the marker ids once; the dead-code subquery then checks `e2.source IN (SELECT id FROM test_marker_ids)`.
- **`ContextBuilder::find_entry_points` deduplicates FTS terms across the five search rounds.** Full query, extracted symbols, stem variants, and agent-provided extra keywords overlap heavily (e.g. `symbol "foo"` and `keyword "foo"` produce identical FTS results); each duplicate term cost a full DB roundtrip on the single-connection libsql. Terms are now collected into one ordered, deduplicated list before any `search_nodes` calls — original priority preserved (full query → symbols → stems → keywords) so the `cap`-based early exit still favours higher-signal terms first.
- **`ContextBuilder` reads each source file at most once per `build_context`.** Both `extract_code_blocks` and `merge_adjacent_blocks` previously called `get_code(node)` which did its own `fs::read_to_string` per call; merging K adjacent blocks meant K disk reads of the same file. Introduced `get_code_cached(node, file_cache)` that consults a shared `HashMap<String, Option<String>>`; `build_context` allocates one cache for the request and threads it through both phases.
- **`has_bare_call` short-circuits lines with no `(` and rejects substring matches on both identifier boundaries.** Common short names like `new` / `get` / `len` triggered `line.match_indices(name)` over the full line and then filtered post-hoc — pathological on comment/docstring lines that mention the name without calling it. Added a `line.contains('(')` fast path plus an after-byte identifier-boundary check (so `new` no longer pre-matches inside `newer`).
- **`get_nodes_by_ids` and friends build their `IN (?, ?, …)` placeholder string in one allocation.** Previous `(1..=ids.len()).map(|i| format!("?{i}")).collect()` allocated one `String` per id and an intermediate `Vec<String>` per call — visible on profiles because `traverse_bfs` invokes `get_nodes_by_ids` once per BFS level. New `build_qmark_placeholders(n)` writes `?, ?, ?, …` into a single pre-sized `String`; libsql binds anonymous markers positionally so behaviour is unchanged. Applied at `get_nodes_by_ids`, `batch_incoming_call_counts`, and `search_nodes_by_exact_name`.

## [4.14.5] - 2026-05-16

### Fixed
- **`tracedecay_inheritance_depth` no longer explodes on cyclic / near-cyclic trait-bound graphs.** The recursive CTE in `get_inheritance_depth` had a depth bound of 50 but no cycle detection, so any cycle in the `extends` graph (common in Rust workspaces where generic trait bounds form indirect cycles) made the CTE traverse the cycle up to the depth limit from every entry point. On polkadot-sdk (959 `extends` edges) the query took >60 s and timed out; smaller workspaces (sotf 89, scirs 90, sonium 5) were fine. Fix tracks visited node IDs in a path column and skips recursion when the next target is already in the path — query completes in 0.55 s on polkadot end-to-end through MCP. Regression test `test_get_inheritance_depth_terminates_on_cycle` constructs a 3-node graph with an A↔B cycle and a C→A edge, then asserts the query returns in <2 s with all three nodes reported at finite, bounded depth.

### Added
- **`scripts/mcp_probe/` — MCP test-matrix harness.** Drives a fresh `tracedecay serve` MCP server over stdio against a configurable set of real repos and exercises every read-only tool with 5 query variants per language, producing a per-tool / per-repo status table that flags tools needing investigation (errors, timeouts, empty results, perf regressions). Same harness doubles as a benchmark — per-call timings are logged, repos serve as a fixed corpus for cross-version perf comparison. Pluggable per-language probe modules under `tools/`; Rust ships included (`tools/rust.py` exercises all 50 MCP tools). `repos.toml` (overridable via `$TRACEDECAY_PROBE_REPOS`) holds the repo set. JSON-RPC ids are strictly matched in `probe.py::McpClient` so a slow call cannot poison subsequent ones; `isolated.py` adds a fresh-server-per-tool retry loop for tools that already showed a real timeout. `build_matrix.py` renders the log to markdown. Used to find and prove this release's `inheritance_depth` cycle bug; same harness verifies the 4.14.4 fixes stay green across the four real repos (sotf, sonium, scirs, polkadot-sdk).

## [4.14.4] - 2026-05-16

### Fixed
- **`tracedecay_doc_coverage` reports public fields, enum variants, constants, statics, type aliases, properties, …** The query previously filtered to `kind IN ('function', 'method', 'class', 'interface', 'trait', 'struct', 'enum', 'module')` — so a Rust file full of `pub` undocumented struct fields reported `total_undocumented: 0` even though `tracedecay_module_api` listed dozens of public symbols on the same file. Kind allow-list expanded to include `field`, `enum_variant`, `const`, `static`, `type_alias`, `property`, `csharp_property`, `record`, `data_class`, `sealed_class`, `object`, `case_class`, `kotlin_object`, `inner_class`, `abstract_method`, `constructor`, `struct_method`, `val`, `var`, `mixin`, `extension`, `union`, `typedef`. Excludes `namespace` and `package` — those are aggregators that almost never carry their own doc and would just drown out actionable items. Single `const` deduplicates the prefix and no-prefix branches. Verified end-to-end on real DBs: `biquad.rs` in sotf went from 0 → 23 undocumented public symbols; polkadot-sdk reports ~51 K with a sensible per-kind breakdown. Regression test `test_get_undocumented_public_symbols_includes_fields_and_variants`.
- **`tracedecay_dead_code` excludes `#[test]`-annotated functions whose name does NOT start with `test`.** The previous filter was name-prefix-only (`name NOT LIKE 'test%'`), so `#[test] fn from_measurement_slope_excludes_lfe()` and similar leaked through. The libtest harness is an implicit caller that never appears as a graph edge, so without this filter most Rust tests with non-`test*` names got misreported as dead. Detection now walks the `annotates` edges and excludes any node whose annotation_usage name is `'test'`, `'…::test'` (covers `tokio::test`, `async_std::test`), `'wasm_bindgen_test'`, or `'…::wasm_bindgen_test'`. The JOIN is constrained to `a.kind = 'annotation_usage'` to avoid accidental matches. Real-DB impact: sotf 1794 → 540 dead functions (-70 %), sonium 778 → 209 (-73 %), scirs 4 839 → 2 469 (-49 %), polkadot-sdk **12 136 → 2 295 (-81 %)**. Manual spot-check on polkadot confirmed every dropped name is unambiguously a `#[test]` function. Regression test `test_find_dead_code_excludes_test_annotated`.
- **`tracedecay_ast_grep_rewrite` surfaces a useful message when `ast-grep` exits non-zero with empty stderr.** ast-grep returns exit 1 with completely silent stdout/stderr when its pattern matches 0 nodes or when the file's language can't be inferred from the extension (`.txt`). The previous error string was `"ast-grep failed: "` — empty trailer, no actionable info. New handler falls back through stderr → stdout → an explicit explanation listing likely causes (pattern matched 0 nodes, language not inferred, invalid pattern), plus the exit code and the file + pattern that failed. Regression test `ast_grep_rewrite_surfaces_useful_error_on_empty_stderr`.
- **`tracedecay_port_status` no longer cross-matches methods that share a name but belong to different parent types.** The match key was `(name.to_lowercase(), kind_compat_group)`, so `Biquad::new` matched `Adaa::new`, `Biquad::process` matched any other `process`, and so on — useless on Rust workspaces where every type has a `new`, `process`, `fmt`, `reset`, etc. Match key now also includes the parent qualifier (stripped of generics — `Biquad<T>` and `Biquad` resolve identically) for kinds that have one (`method`, `field`, `enum_variant`, `struct_method`, `abstract_method`, `constructor`, `csharp_property`, `property`, `val`, `var`); top-level kinds (struct, function, enum, trait) keep name-only matching since their containing context in `qualified_name` is just a file path. Regression tests `port_status_does_not_match_methods_of_different_parents` (Biquad in dir A, Adaa in dir B — must NOT match) and `port_status_matches_methods_with_same_parent_type` (Biquad in both dirs — must match).
- **`tracedecay_branch_diff` returns an empty diff when base == head instead of erroring.** Previous behaviour was `MCP error -32603: base and head are the same branch`, inconsistent with `tracedecay_pr_context` which already handled the same case by returning empty arrays. Same-ref now returns the normal JSON shape with `summary: {added:0, removed:0, changed:0}`, empty `added`/`removed`/`changed` arrays, and a `note` field explaining the equality — so callers can rely on a single response shape. Regression test `branch_diff_returns_empty_when_base_equals_head`.

## [4.14.1] - 2026-05-15

### Fixed
- **`tracedecay_search` always ranks definitions above `use` re-exports.** BM25 was scoring short `pub use crate::operator::LinearOperator;` rows highly enough that five re-exports outranked the actual `pub trait LinearOperator { … }` definition. Sort now uses a coarse `kind_tier` as the primary key (defs tier 0, impl tier 1, values/members tier 2, modules tier 3, `use`/`include`/annotation usage tier 4); BM25 score is secondary within a tier. Added a per-row exact-name match boost (+10) so a trait named exactly `Foo` beats a `Method` whose qualified name happens to contain `Foo`. Regression test `search_ranks_trait_definition_above_use_reexports` constructs a trait plus five `pub use` re-exports across sibling modules and asserts the trait is hit #1.
- **`tracedecay_dead_code` no longer treats `annotates` / `derives_macro` / `contains` edges as "this function is alive" evidence.** Real-world Rust pervasively uses `#[inline]`, `#[derive(Debug)]`, and similar attributes — every annotation_usage node creates an `annotates` edge pointing at the function, which the previous `kind != 'contains'` filter accepted as a live reference. Result on the sonium codebase: 0 dead functions across 5,715. Narrowed the SQL filter to an explicit allowlist of real-use edges: `calls`, `implements`, `extends`, `type_of`, `returns`, `receives`, `uses`. Regression test `dead_code_flags_unreferenced_fn_with_attribute` exercises the `#[inline]` case.
- **`tracedecay_unused_imports` handles grouped imports (`use std::collections::{HashMap, HashSet};`).** The previous parser treated the literal string `{HashMap, HashSet}` as one identifier and never matched it against the file body, so every grouped import was effectively ignored — explaining the user's "0 unused across 3,404 use nodes" report. A new `identifiers_from_use_path` helper splits grouped/aliased/nested forms (`foo::bar`, `foo::bar as baz`, `foo::{a, b as c}`, `foo::{a, nested::b}`, `foo::{self, bar}`), and the handler now reports one entry per truly-unused identifier with an `unused: <identifier>` field. Regression test `unused_imports_handles_grouped_use` verifies the unused half of a grouped use is flagged while the used half is not.
- **`tracedecay_changelog` filters deleted-subtree directory entries.** When an entire subtree was removed in a diff, gix yielded a directory-mode deletion entry whose path was gone from disk by the time the post-hoc `is_dir()` check ran — so directories like `crates/sonium-bem` slipped through as `removed_or_not_indexed`. `git_diff_files` now inspects `entry_mode.is_tree()` on each gix `Change` record (addition/modification/deletion/rewrite) and never pushes a tree entry into the changed-files list. The disk-based `is_dir()` filter is kept as belt-and-suspenders for additions/modifications. Regression test `changelog_filters_deleted_directory_entries` synthesises a `git rm -r crates/` commit and asserts no non-`.rs` paths appear in `changed_files`.
- **`tracedecay_diff_context.modified_symbols` dedupes by node id and dedupes the input `files` array.** Callers that synthesised the file list from upstream tooling (directory walks, multi-source mergers) sometimes passed the same path multiple times — `hmatrix.rs` was reported up to 7× in a row. Added a `modified_seen: HashSet<String>` to guard pushes and an early `files` dedup pass. Regression test `diff_context_dedupes_modified_symbols_on_duplicate_input` passes the same path three times and asserts unique node ids in the output.
- **`tracedecay_pr_context` collapses Cargo.toml into a single `config_summary` entry.** Behaviour was already present; added the regression test `pr_context_collapses_cargo_toml_keys` which synthesises a real git history with a 50-dependency Cargo.toml bump and asserts at most one Cargo.toml entry surfaces (kind = `config_summary`).
- **`tracedecay_circular` SCC disjointness stress test.** Added `circular_emits_disjoint_sccs_under_load` — five 3-file cycles connected by non-cyclic DAG-style tails — to guard against any future SCC implementation drift that might let a file leak into more than one cycle entry.

### Added
- **`tracedecay_port_order` surfaces intra-cycle ordering signals.** Each cycle entry now reports per-symbol `in_cycle_in_degree` and `in_cycle_out_degree`, a file-level `members_in_cycle` breakdown ranked by member count, an explicit `entry_point` (the SCC member with the smallest in-cycle out-degree — leaf-most, the natural starting point), and a `break_point_candidate` (the highest in-cycle in-degree node, the hub whose call sites are the most-effective refactor target). Replaces the previous flat blob of 200+ symbols with no guidance on where to start. Regression test `port_order_provides_intra_cycle_ordering` wires a 4-node SCC with one obvious hub and asserts `break_point_candidate.name == "h"`.

### Changed
- **`tracedecay_ast_grep_rewrite` is conditionally registered.** The tool is only advertised via `tools/list` when the external `ast-grep` binary is on PATH at server-startup time (cached via `OnceLock` so we don't fork on every `tools/list` request). When the binary is missing, models never see a tool that would immediately return "ast-grep is not installed" on first call. `tracedecay::mcp::tools::ast_grep_available()` is now public; tests in `mcp_handler_test::test_tool_definitions_complete` and `mcp_test::test_tool_definitions_count` branch on it so they pass on hosts with or without the binary installed.

## [4.14.0] - 2026-05-15

### Fixed
- **`tracedecay_run_affected_tests` dispatches directly-changed test files.** Previously the handler only walked callers of every node in `changed_paths` — `#[test]` functions are leaves with no callers, so a PR that only touched `tests/foo.rs` returned "no tests cover the changed paths" and skipped running anything. The handler now also dispatches test functions whose file is itself in `changed_paths` (either via `is_test_file` path heuristic or `#[test]` annotation), with the test recorded as covering itself in `covers_source_ids`.
- **`parse_derives_in_attr_block` handles rustfmt's multi-line derive blocks.** The previous line-bounded scanner only matched `#[derive(...)]` when the closing `)` was on the same line, so rustfmt's split form (`#[derive(\n    Debug,\n    Clone,\n)]`) dropped every derive. The parser now joins the attribute-block lines and scans for `#[derive(` ... `)` across the whole region. Two new unit tests (`parses_multiline_derive_attribute`, `parses_multiline_derive_mixed_with_single_line`) cover the split form.
- **`tracedecay_diagnose` normalises absolute and backslash paths.** Cargo emits absolute spans when `--manifest-path` points outside cwd, and Windows cargo emits backslash-separated paths; neither matches the indexed forward-slash, project-relative form. `node_at_location` now calls a new `normalize_lookup_path` helper that (1) replaces `\` with `/`, (2) strips the canonicalised project-root prefix for absolutes, and (3) falls back to a raw prefix strip when canonicalisation fails. A diagnostic spanning either form now maps to the correct node.
- **Resolver kind-compatibility filter now applies to the same-file blocklist branches (bug #11 follow-up).** PR8's filter was wired into the main `try_exact_name_match` / `try_qualified_match` paths but not the two `CROSS_FILE_BLOCKLIST` branches in `try_exact_name_match` and `try_exact_name_match_simple`. Common blocklisted names (`new`, `default`, `clone`, …) could still bind a `Calls` reference to a same-file non-callable — a struct or const sharing the name. Both branches now filter candidates through `kind_compatible` before declaring a same-file match. Regression test `resolver_blocklist_branch_respects_kind_filter` reproduces the case (`struct new` + `caller() { let _ = new(); }`) and asserts callees only include callable kinds.

## [4.13.0] - 2026-05-15

### Fixed
- **Resolver kind-compatibility filter (bug #11)** — `tracedecay_rank --edge-kind implements` (and every downstream tool: `tracedecay_impls`, `tracedecay_type_hierarchy`, `tracedecay_callees`'s trait dispatch, …) was poisoned by the resolver fuzzy-binding `impl Default for X` to whatever local node happened to share the name `Default`. The sonium codebase had a parser `Token` enum with a `Default` variant; 150 manual `impl Default for X` blocks all bound to that one `enum_variant`, swamping the rank tool with junk.
- New `kind_compatible(ref_kind, target_kind)` helper in `src/resolution/resolver.rs` enforces a structural matrix:
  - `Implements` / `Extends` / `DerivesMacro` → must target trait/interface/class/abstract-method/sealed-class/annotation/type-alias kinds
  - `Calls` → must target a callable (function/method/struct-method/constructor/abstract-method/arrow-function/procedure/macro)
  - `Annotates` → must target annotation/decorator kinds
  - `Uses` / `TypeOf` / `Returns` / `Contains` / `Receives` → permissive (any kind)
- Both `try_qualified_match` and `try_exact_name_match` now apply the filter; when filtering shrinks the candidate list, a `resolve_from_filtered` helper picks the same-file candidate first then falls back to the first overall, with confidence reduced to reflect the partial match. This prevents the previous "any-name-wins" behaviour without dropping legitimate resolutions.
- Regression test `implements_refs_dont_resolve_to_enum_variants` constructs the exact sonium-style scenario (`enum Token { Default, Plus }` plus two manual `impl Default for X` blocks) and asserts that `tracedecay_rank --edge-kind implements` does NOT list an `enum_variant` or `field` target. Existing DBs need `tracedecay sync --force` to re-resolve refs under the new constraints.

## [4.12.0] - 2026-05-15

### Added
- **`src/graph/scc.rs` — Tarjan's strongly-connected-components algorithm.** Iterative (no recursion, no stack-blow risk on deep graphs), generic over node-id type, returns components in reverse-topological order matching what port ranking needs. Used by both `tracedecay_circular` and `tracedecay_port_order`. Five unit tests cover DAGs, two-node cycles, three-cycle-plus-tail, self-loops, and reverse-topo emission order.

### Fixed
- **`tracedecay_circular` reports one entry per SCC, not per DFS walk (bug #10)** — the previous implementation emitted every distinct DFS path through a cycle, producing 73 overlapping cycle entries on the sonium codebase that all shared a long common tail. `find_circular_dependencies` now computes SCCs via Tarjan and emits one entry per genuine mutually-recursive group, filtering out trivial single-node components that don't have self-loops. The legacy `dfs_cycle_detect` helper and `_legacy_walk_cycles` shim were removed.
- **`tracedecay_port_order` exposes per-SCC cycle groups (bug #12)** — previously, every unsorted node after Kahn's topological sort was lumped into a single "Mutual dependency — port together" entry, so two disjoint mutually-recursive pairs `(a,b)` and `(c,d)` would render as one mega-cycle and lose all signal. The handler now runs Tarjan on the subgraph of unsorted nodes and emits one cycle entry per non-trivial SCC, with the `files` set of each cycle surfaced so the user has a concrete "break this edge" target. Each entry carries `symbols`, `files`, `size`, and a refined `note`.

## [4.11.0] - 2026-05-15

### Fixed
- **`tracedecay_dependency_depth` no longer follows `implements`/`extends` edges (bug #7)** — the resolver fuzzy-binds `impl Debug for T` and similar across unrelated files, producing chains of spurious file-to-file deps (the report observed a 19-level chain spanning 17 unrelated files terminating in a foreign crate). `build_file_adjacency` now follows only `calls` and `uses` edges. Existing `tracedecay_health` and `tracedecay_circular` callers benefit too — they share the same adjacency builder.
- **`tracedecay_dead_code` no longer reports 0 on `pub`-heavy codebases (bug #8a)** — two fixes: (1) the `NOT EXISTS` subquery now excludes `Contains` edges, which previously masked every node behind its parent's bookkeeping edge; (2) new `include_public: true` argument opts into auditing pub items with no indexed callers, useful for workspace-internal cleanup. Default behaviour (no flag) still excludes pub items as before.
- **`tracedecay_unused_imports` no longer returns 0 on real codebases (bug #8b)** — the previous graph-only check tested `incoming.is_empty()`, but every Use node has at least one Contains edge from its parent, so the predicate never fired. New heuristic reads the source file once (cached per file) and checks whether the imported identifier appears as a whole-word token outside the use statement itself; matches what `cargo`'s own unused-import lint does. `pub use` re-exports, glob imports, and `use self::...` are skipped (intentional aliases / out-of-scope for textual heuristics). Three regression tests cover unused-detection, the dead-code Contains-edge bug, and the new `include_public` opt-in.

### Changed
- **`TraceDecay::find_dead_code` signature** — gained an `include_public: bool` parameter. Existing callers (`tracedecay_health`, internal tests) updated to pass `false` to preserve previous semantics.

## [4.10.0] - 2026-05-15

### Fixed
- **`tracedecay_body` prefers callable kinds over same-named fields (bug #1)** — sonium hit a case where querying `gmres` returned only a struct field literally named `gmres` and missed the obvious `pub fn gmres(...)`. The handler now does an exact-name DB lookup first (via the PR1 suffix-fallback path) so the function isn't buried under BM25 noise, then sorts matches by `body_kind_preference()`: callable (0) > type def (1) > impl (2) > value (3) > field/variant (4) > use (5).
- **`tracedecay_changelog` / `commit_context` / `pr_context` no longer list directories (bug #4)** — gix's `for_each_to_obtain_tree` yields directory-level entries when an entire subtree changes. `git_diff_files` now filters out any path that resolves to a directory on disk, so callers see only file paths.
- **`tracedecay_diff_context.impacted_symbols` dedupes by node id (bug #5)** — diamond dependencies caused the same downstream node to appear 6+ times consecutively. `impacted_seen: HashSet<String>` now guards inserts.
- **`tracedecay_recursion` drops length-1 self-cycles (bug #6)** — single-node cycles are almost always either resolver fuzzy-binding (`self.push()` cross-bound across distinct impls of the same name) or trivial self-recursion. Cycles with `< 2` distinct nodes are now filtered out before being added to the result set.
- **`tracedecay_commit_context` / `tracedecay_pr_context` collapse config-file symbols (bug #3)** — Cargo.toml's 50+ dependency keys used to each enumerate as a separate "modified symbol", blowing past 50K tokens on a real diff. Both handlers now emit a single `{kind: "config_summary", file, config_keys: N}` entry per file with role `config` (`*.toml` / `*.yaml` / `*.json` / `*.ini` / `*.cfg` / `*.lock`).
- **`classify_file_role` no longer flags source files with inline tests as "test" (bug #3 follow-up)** — a `src/foo.rs` with `#[cfg(test)] mod tests` at the bottom keeps role `source`. The "test" bucket is reserved for files that exist purely to host tests (path-based check via `is_test_file`). Three unit tests in `mcp::tools::handlers::git::tests` cover the classification matrix.
- **Rust extractor emits `Extends` edges for supertrait bounds (bug #9)** — `trait Leaf: Middle + Base` now produces unresolved refs with `EdgeKind::Extends` for each bound, so `tracedecay_inheritance_depth`'s recursive CTE walks Rust supertrait chains correctly. Bound extraction handles `type_identifier`, `scoped_type_identifier`, `generic_type`, and `higher_ranked_trait_bound`. Existing DBs need a re-index (`tracedecay sync --force`) to pick up the new edges.

## [4.9.0] - 2026-05-15

### Added
- **`tracedecay_derives` tool** — surfaces the `#[derive(...)]` macros attached to a type plus the trait + method names each one synthesizes. Closes the dead-end-search gap where calls like `.clone()`, `format!("{:?}", x)`, or `serde_json::to_string(&x)` resolve to methods that never appear in the graph (the impl is generated by the proc-macro at compile time). Accepts either `qualified_name` or `node_id`. Well-known derives carry full info (trait path, method list, source crate); unknown / proc-macro derives surface with `well_known: false` and just the derive name.
- **`derive_table` module** (`src/derive_table.rs`) — static knowledge of well-known derives (`Debug`, `Clone`, `Copy`, `Default`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash`, `Serialize`, `Deserialize`, `Display`, `Error`), each mapped to its canonical trait path and method names. Five unit tests cover known + unknown derives and the `enrich` wrapper.
- **`derives` field on `tracedecay_node` output for type nodes** — when the queried node is a `Struct` / `Enum` / `Union` / `Record` / `CaseClass` / `DataClass` / `PascalRecord`, the response now includes a `derives` array so callers don't need a second roundtrip just to learn what derives are present.
- **`TraceDecay::get_derives_for_node(node_id)`** — public helper that re-reads the node's source-file attribute block and parses `#[derive(...)]` directly. The graph's `DerivesMacro` edges are unreliable: the resolver fuzzy-binds std-trait names like `Debug` to nonsense targets (e.g. a `Debug` enum variant in an unrelated test fixture), and the unique constraint on `(source, target, kind, line)` then collapses multiple derives on the same type onto a single edge. Re-parsing from source costs one `fs::read` per node lookup (cheap at typical Rust source sizes) and recovers the full derive list. Five unit tests in `derive_parse_tests` cover single/multi-block derives, qualified paths, mixed attribute kinds, and dedup.

### Changed
- **Total MCP tools: 59 → 60** — `tracedecay_derives` added.

## [4.8.0] - 2026-05-15

### Added
- **`tracedecay_diagnose` tool** — parses raw `cargo check` / `cargo clippy` / `rustc` stderr into structured diagnostics, then maps each one to the smallest containing graph node and (by default) pre-attaches up to 5 callers. Closes the today-an-agent-hand-parses gap: the response includes severity, optional error code (`E0308`, clippy lint name), message, file/line/column, the owning node (id, kind, qualified_name, span), and the call sites the broken code is reachable from. Diagnostics without a `--> file:line:col` span are dropped — they cannot be located. Accepts a `severity` filter (`error` / `warning` / `all`) and a `max_diagnostics` cap (default 50, hard cap 500).
- **`tracedecay_run_affected_tests` tool** — closes the loop opened by `tracedecay_test_map` / `tracedecay_test_risk`. Given `changed_paths` (or, by default, `git diff --name-only HEAD`), the handler walks the graph to find every test that covers a function/method in those files, then runs `cargo test --no-fail-fast -- <names>` with `kill_on_drop` and a configurable `timeout_secs` (default 300). Parses libtest stdout into JSON `{ test, passed, covers_source_ids[] }` entries plus pass/fail counts and the cargo exit code; trailing stdout/stderr are tailed at 2 KB each so the response stays in budget. `max_tests` defaults to 100 (hard cap 500) so a refactor touching everything doesn't dispatch an unbounded list.
- **`src/diagnose.rs`** — standalone parser module. Five unit tests cover typed errors (`error[E0308]`), clippy-style headers without codes, summary lines without spans (correctly dropped), multi-diagnostic blocks, and ANSI-prefixed lines.
- **`TraceDecay::node_at_location(file, line_1based)`** — public helper that returns the smallest-span node containing a 1-based source location. Used by `tracedecay_diagnose`; converts to the internal 0-based representation transparently.

### Changed
- **Total MCP tools: 57 → 59** — `tracedecay_diagnose` and `tracedecay_run_affected_tests` added.
- **New handler module `src/mcp/tools/handlers/workflow.rs`** — keeps cargo/libtest plumbing out of `graph.rs`, which is for code-graph queries.

## [4.7.0] - 2026-05-15

### Added
- **`tracedecay_impls` tool** — index of `impl Trait for Type` blocks. Accepts optional `trait` and `type` filters (both short and qualified names). With neither, returns every impl in the graph. Surfaces information that was previously buried behind the second-class `Implements` edge: which types satisfy a given trait, which traits a type implements, and the impl blocks themselves with their files and signatures.
- **Trait dispatch resolution on `tracedecay_callees`** — when a callee resolves to a method whose enclosing scope is a trait, the handler walks back via `Implements` edges to surface the concrete impl methods reachable through that trait. New entries are tagged `dispatch_via_trait: true` and carry a `dispatch_from` pointer to the trait method. Pass `resolve_dispatch: false` to opt out and get only direct call edges.
- **`TraceDecay::get_impls(trait, type)`** — public helper backing the new tool.
- **`TraceDecay::get_trait_dispatch_targets(method)`** — public helper that returns every impl-method satisfying a given trait method, used by `handle_callees` to surface dispatch targets.

### Changed
- **Total MCP tools: 56 → 57** — `tracedecay_impls` added.
- **`tracedecay_callees` description and schema** updated to advertise dispatch resolution and the new `resolve_dispatch` argument.

### Fixed
- **`tracedecay_search` ranks definitions above references (PR1 follow-up)** — BM25 alone was placing `use foo` statements ahead of the actual `pub fn foo()` definition because both score similarly when the symbol name matches. `TraceDecay::search` now over-fetches and re-ranks: every `NodeKind` carries an explicit bonus (callable defs +3.0, type defs / proto defs +2.5, impl blocks +2.0, values / macros / enum variants +1.0, members +0.5, neutral 0.0, container modules -1.5, annotation usages -2.0, `use` / `include` -3.0). The match is exhaustive so adding a new `NodeKind` forces a re-tune here. Result: searching for `gmres` returns the function before its imports.
- **`get_nodes_by_qualified_name` falls back to suffix or bare-name match (PR1 follow-up + user feedback)** — strict equality match remains primary. On empty results: queries with `::` retry as `qualified_name LIKE '%::<query>'` (full scan, `LIMIT 50`); queries without `::` retry as `name = ?` using `idx_nodes_name`. Both forms now resolve, e.g. `get_impls`, `TraceDecay::get_impls`, and the full doubled path all return the same row. `tracedecay_signature` and `tracedecay_by_qualified_name` share the lookup so they agree.
- **Rust extractor no longer doubles the file path in `qualified_name`** — `qualified_prefix()` prepended `self.file_path` even though the file root was already pushed onto `node_stack` at extraction start, producing qnames like `src/foo.rs::src/foo.rs::Type::method`. Now iterates the stack only, yielding `src/foo.rs::Type::method`. Existing DBs will keep the old form until re-indexed (`tracedecay sync --force`).
- **`get_impls` batches the trait lookup (PR2 review follow-up)** — previously one `get_node_by_id` per impl block (N+1). Now collects every Implements-edge target then issues a single `get_nodes_by_ids` to populate the trait map.
- **`graph_stale` insertion asserts on non-object results (PR1 review follow-up)** — `handle_tools_call` now `debug_assert!`s that the wrapped tool result is a JSON object before attaching the `graph_stale` field, matching the "crash hard on unknown value" convention so a future handler returning a non-object is caught immediately instead of silently dropping the structured staleness signal.
- **`cost_to_expand` body heuristic documented as Rust-tuned (PR1 review follow-up)** — the `20 tokens/line` rate over-estimates Haskell/Python by ~2-3x; the doc comment now explicitly says so and notes the single-line floor of 20 tokens, since this number is part of the public tool contract.

## [4.6.0] - 2026-05-15

### Added
- **`tracedecay_signature` tool** — signature-only lookup by `qualified_name` or `node_id`. Returns visibility, signature string (generics, params, return type, where clauses), docstring, kind, and async flag for matching nodes. No body content. Replaces most agent `Read` calls when only the public-API surface of a symbol is needed.
- **`graph_stale` field on tool results** — when files referenced by a tool result remain stale after the post-call sync attempt, the JSON-RPC response now carries a top-level `graph_stale: ["path", …]` array plus a machine-parseable `tracedecay_graph_stale: [...]` text marker. The existing human-readable WARNING is preserved. Closes the silent-drift gap where renamed/deleted symbols could return phantom callers/callees without a programmatic signal.
- **`cost_to_expand` annotation on node results** — `tracedecay_node` and `tracedecay_signature` responses now include `cost_to_expand: { body, full_file }` (approximate tokens) so callers can decide whether to set `include_code=true` before re-querying. Body estimate uses ~20 tokens/line; `full_file` uses indexed `files.size / 4`.
- **`tracedecay://schema` MCP resource** — markdown resource documenting the on-disk `.tracedecay/tracedecay.db` schema: tables, columns, indexes, FKs, common query recipes (impl-of-trait, top callers, largest functions), and gotchas (content-hashed IDs, trait dispatch, derive macros). Makes the SQLite escape hatch usable without trial-and-error.
- **`TraceDecay::get_file_size_bytes(path)`** — public helper that returns the indexed byte size of a file (0 when unknown). Backs the `cost_to_expand` full-file estimate.

### Changed
- **Total MCP tools: 55 → 56** — `tracedecay_signature` added; all existing tools unchanged.

### Fixed
- **Clippy: project-wide cleanup to restore `-D warnings`** — 43 pre-existing lib errors and 3 bin errors resolved without behavioral change: module doc comments wrap snake_case tool names in backticks; `bench.rs` uses `write!` instead of `format!(..).push_str`; `extraction_worker.rs` converted to `let…else`; redundant closures in `agents/copilot.rs`, `extraction/haskell_extractor.rs`, `mcp/tools/handlers/memory.rs` replaced with method references; `resolution/resolver.rs` merges identical match arms; `serve.rs` uses `sort_by_key`; `upgrade.rs` uses `is_ok_and`; `main.rs` drops a useless `.into()`.

## [4.5.1] - 2026-05-15

### Added
- **`tracedecay monitor` highlights the last 3 updates** — the most recently active (project, tool) pair renders green, second-to-last orange, third-to-last yellow. Re-firing the same tool moves it to the front rather than duplicating. Cleared on Ctrl+R.
- **Welcome banner on fresh installs** — when `tracedecay` is invoked with no subcommand and the global DB has zero registered projects, print a cyan welcome that explicitly suggests `tracedecay init` before the existing "Create one now?" prompt. Returning users see no change.

### Fixed
- **CI: `clippy::items_after_test_module` denied under Rust 1.95.0** — two test modules from the v4.5.0 work (`gain_tests` in `src/commands.rs`, `gain_format_tests` in `src/display.rs`) were inserted mid-file. Rust 1.95.0 promoted this lint into `clippy::all`, which the project denies project-wide. Both moved to file end.
- **CI: `cargo fmt` drift across 11 files** — accumulated unwrapped one-line `println!` / `match` / struct literals from the v4.5.0 features; re-formatted to match `rustfmt` expectations.

## [4.5.0] - 2026-05-15

### Added
- **Cross-session memory primitives (3 new MCP tools)** — `tracedecay_record_decision`, `tracedecay_record_code_area`, and `tracedecay_session_recall` persist agent decisions and worked-on paths in the per-project DB so they survive across sessions. `session_recall` uses FTS5 for fuzzy retrieval. Backed by two new tables and an FTS mirror added in schema migration v8.
- **`tracedecay gain` CLI for the savings ledger** — every MCP tool call now writes an append-only row to a new `savings_ledger` table in the global DB. `tracedecay gain [--all] [--history] [--range 7d] [--json]` reports tokens saved + dollar estimates (Sonnet input pricing, refreshed daily via LiteLLM).
- **`tracedecay bench` reproducible retrieval benchmark** — runs a fixed query set through `tracedecay_context` and reports retrieval savings vs a full-file baseline (CCE-style methodology). Ships with a 10-query generic default set embedded into the binary (no external file dependency); `--queries <file>` accepts a custom set. Measured **93% mean retrieval savings on tracedecay's own repo** (180K → 3.4K tokens across 10 generic queries).

### Changed
- **Schema bumped from v7 to v8** — adds `memory_decisions`, `memory_code_areas`, and the `memory_decisions_fts` virtual table. Existing user DBs upgrade idempotently via `migrate_v8`; fresh installs use the mirrored DDL in `create_schema`. No breaking changes; existing tools and queries continue to work.
- **`GlobalDb::open()` refactored to delegate to `GlobalDb::open_at(path)`** — enables test isolation via `tempfile::TempDir` without process-wide `HOME` mutation. The public `open()` API is unchanged.
- **Total MCP tools: 52 → 55** — three new memory tools added; all existing tools unchanged.

### Fixed
- **`coverage_discipline` health penalty reduced from 10% to 2% (issue #76)** — annotating genuinely untestable functions with `/// skip-test-coverage` was dropping `quality_signal` despite improving `coverage_pct`, because the penalty had no positive counterbalance (coverage doesn't feed into the composite health score). Max penalty reduced so honest annotation is not punished.

## [4.4.0] - 2026-05-14

### Fixed
- **Rust extractor now detects function calls inside macro invocations (issue #72)** — `assert!(check_count(5))` previously only created a Calls edge to `assert`, missing `check_count`. The extractor now walks into `token_tree` nodes inside macros to find nested call patterns.
- **`test_risk` denominator no longer includes test functions (issue #73)** — functions with `#[test]` annotations and functions inside `::tests::` modules are now excluded from `total_functions` and the coverage percentage denominator.
- **Rust extractor resolves instance method calls (issue #74)** — `instance.method()` now emits an additional unresolved ref with just the method name, allowing the resolver to match it against `impl` method definitions. Previously only associated function calls (`Type::new()`) were resolved.

### Added
- **`/// skip-test-coverage` doc comment convention (issue #75)** — mark genuinely untestable functions to exclude them from `test_risk` coverage calculations. The `skipped` count appears in the summary. A `coverage_discipline` health dimension penalises overuse (up to 10% quality signal reduction).
- **VS Code Insiders support for the Copilot installer (issue #69)** — `tracedecay install --agent copilot` now also configures `Code - Insiders/User/settings.json` alongside the regular VS Code path.
- **Copilot prompt instructions (issue #70)** — the Copilot installer now writes `copilot-instructions.md` with tracedecay MCP tool guidance to VS Code (`User/prompts/`), VS Code Insiders, and Copilot CLI (`~/.copilot/`).

## [4.3.18] - 2026-05-14

### Fixed
- **Inline `#[cfg(test)]` test modules are now recognized as test coverage** — `test_map`, `health`, `test_risk`, `affected`, `impact`, and `commit_context` previously only detected tests by file path patterns (`tests/`, `_test.`, etc.). Functions annotated with `#[test]` inside inline `#[cfg(test)] mod tests { ... }` blocks in source files (226 such functions in tracedecay's own codebase) were invisible to coverage analysis. The Rust extractor now emits `Annotates` edges from `#[cfg(test)]` to modules, and all test-detection handlers query `#[test]` annotations via the graph in addition to checking file paths.
- **`tracedecay serve` resolves the correct project in multi-folder workspaces (issue #66 reopened)** — when multiple projects are registered in the global DB, the `serve` fallback now picks the project closest to cwd (ancestor match first, then descendant match) instead of failing with an ambiguity error. As a last resort, the server peeks at the MCP `initialize` request's `roots` array to discover the workspace folder the client is working in.

## [4.3.17] - 2026-05-14

### Fixed
- **`tracedecay upgrade` no longer breaks Homebrew installs (issue #67)** — previously, self-upgrading a Homebrew-managed install mutated the Cellar directly, leaving Homebrew's recorded keg state inconsistent and causing later `brew upgrade` to fail. `tracedecay upgrade` now detects Homebrew installs and delegates to `brew update && brew upgrade tracedecay`. (PR #68, thanks @lesbass)
- **Exclude globs now match nested directories (issue #64)** — the default `node_modules/**` pattern only excluded top-level `node_modules/`, not nested ones like `projectA/node_modules/`. Changed default to `**/node_modules/**`. Also added `is_excluded_dir()` so bare patterns like `**/dist` correctly prune directories during scanning without requiring a trailing `/**`.
- **VS Code multi-folder workspaces can now start the Copilot MCP server (issue #66)** — the Copilot config used `${workspaceFolder}` which VS Code cannot resolve in multi-folder workspaces. Dropped in favour of the serve command's built-in project discovery, matching every other agent integration.

## [4.3.16] - 2026-05-11

### Fixed
- **Windows CI failure introduced by v4.3.15's zed regression test** — `test_zed_install_preserves_existing_config` seeded `AppData/Roaming/Zed/settings.json` on Windows, but `zed_config_dir` actually uses `.config/zed/settings.json` on every non-macOS platform (Linux *and* Windows). The hand-written `#[cfg(target_os = "windows")]` branch in the test silently diverged from the production helper, so the test wrote the seed to one path and the install wrote to another — backup never appeared at the seeded location and the test failed. The Windows job (Linux passed, since its branch happened to be correct) was the only one to catch the drift.

### Changed
- **`AgentIntegration::primary_config_path(home) -> Option<PathBuf>`** — new trait method that returns the single config file the integration rewrites on install/uninstall. Every agent that goes through `safe_write_json_file` or `write_toml_file` implements it (claude, gemini, cursor, opencode, zed, cline, roo-code, copilot, kilo, antigravity, codex); vibe leaves the default `None` because its TOML config is append-only and has no rewrite path. Regression tests in `tests/agent_test.rs` now call `agent.primary_config_path(home)` instead of duplicating platform-conditional path logic — the production helper is the single source of truth, so a future `zed_config_dir`-style change can't drift between tests and reality. A meta-test (`test_every_tested_agent_advertises_primary_config_path`) walks every integration covered by the install regression suite and asserts the method returns `Some(path)` under the test home, so a new integration added without wiring it up fails fast with a clear message instead of producing a confusing missing-backup panic later.

## [4.3.15] - 2026-05-11

### Fixed
- **Installing the Codex integration no longer wipes `~/.codex/config.toml` (issue #63)** — `load_toml_file` used `contents.parse::<toml::Value>()`, which in the `toml = "1"` crate parses a single TOML *value* rather than a *document*. Any well-formed `config.toml` therefore parsed as an error and silently fell back to an empty table; `install_mcp_server` then serialized that empty-plus-tracedecay table back over the file, erasing every other key the user had set (model, approval_policy, other `[mcp_servers.*]` entries, comments). `load_toml_file` now uses `toml::from_str::<toml::Table>` so real documents round-trip, returns `Result` instead of swallowing errors, and refuses to overwrite when an existing file cannot be parsed (so a typo or partial edit leaves the original intact for the user to fix). `doctor_check_config`, `install_mcp_server`, `uninstall_mcp_server`, and `CodexIntegration::has_tracedecay` were updated to handle the `Result` shape — the doctor now reports parse errors as a failed check, and `has_tracedecay` returns `false` on parse error rather than panicking.

### Changed
- **Every config-file write across all agent integrations now leaves a `.bak` copy first.** Previously only install paths went through `backup_config_file`; uninstall paths and `doctor` auto-repair paths called `std::fs::write` directly, so a corrupted serialization or a bug in the rewrite logic could destroy the user's settings with no recovery. A new shared `backup_and_write_json` helper (in `src/agents/mod.rs`) wraps `backup_config_file` + `safe_write_json_file` with best-effort error handling suited to uninstall flows. Every agent's uninstall path (claude, cursor, copilot, cline, zed, kilo, roo-code, opencode, gemini) now goes through this helper, as do the claude `doctor` auto-repair and local-settings-cleanup paths. The Codex TOML write path (`write_toml_file`) also creates a `.bak` before writing for the same reason. Eight per-agent install-side regression tests plus a cursor uninstall-side regression test were added to `tests/agent_test.rs` to guard the new invariant.

## [4.3.14] - 2026-05-11

### Fixed
- **`tracedecay_body` no longer drops the function's outer closing brace (issue #62)** — `handle_body` returned the source spanning `start_line..end_line`, but stored line fields are tree-sitter rows (0-based) while `extract_lines` was written assuming 1-based inclusive inputs. The mismatch meant `lines[start..end_line]` exclusive — one short, lopping off the trailing `}` (or any language's outer block closer sitting on its own line). Inner braces were unaffected because they were never on the boundary. `extract_lines` now treats inputs as 0-based row indices and slices inclusively, so the returned body is byte-exact usable as an `Edit` tool `old_string`. Regression added in `test_body_returns_full_function_source` (`tests/mcp_handler_test.rs`) — verified failing pre-fix with `body: "\nfn format_greeting(name: &str) -> String {\n    format!(\"Hello, {}!\", name)"` (closing `}` missing).

### Changed
- **`tracedecay_body` now exposes `start_line` / `end_line` as 1-based file line numbers** — they were previously the raw 0-based tree-sitter row indices, which read as "off by one" against the line numbers any editor or `Edit`-style tool displays. The values now match what users see when they open the file, so the reported `end_line` is the line containing the function's closing brace. The shift is local to `handle_body`; other handlers still expose `node.start_line` as-is.

## [4.3.13] - 2026-05-10

### Changed
- **Switched to `tree-sitter-grammars/tree-sitter-markdown` (block + inline split parsers)** — the previously-vendored `ikatyang/tree-sitter-markdown` (last updated 2023, GLR-heavy without native frontmatter handling) hung the indexer on otherwise-fine markdown files containing YAML frontmatter. Specifically, the old grammar parsed `---\n…\n---` content as ordinary markdown, where 6/8/10-space-indented YAML lines were simultaneously valid as both deeply-nested list-item continuations and as indented code blocks; tree-sitter's GLR explored all alternatives in parallel, with the surviving-versions count growing exponentially per line. A real-world 18 KB resume.md hung the worker indefinitely; a 4.4 KB minimal reproducer was bisected and is now a regression fixture (`crates/tracedecay-code-extraction/tests/fixtures/markdown_yaml_frontmatter_hang.md`). The new grammar emits an opaque `(minus_metadata)` / `(plus_metadata)` node for frontmatter, so the markdown rules never see the YAML — the same 4.4 KB reproducer parses in ~7 ms, the full 18 KB file in ~16 ms. The markdown extractor was rewritten for the new AST (block parser produces `(atx_heading … heading_content: (inline …))`, headings still become `Module` nodes; the inline parser is run over each `(inline)` byte range via `set_included_ranges` to extract `(inline_link)` for `Uses` edges). All 16 existing markdown extraction tests still pass; 3 new regression tests guard the migration.

### Added
- **Per-file extraction timeout** — every extractor round trip is now wrapped in a watchdog (configurable via `extraction_timeout_secs` in `~/.tracedecay/config.toml`, default 60 s). A file whose extractor doesn't respond in time has its worker subprocess killed via `Child::kill()` and is recorded in `SyncResult.skipped_paths` with reason `"extractor timed out (>Ns)"`. Worker crashes (the existing failure path) are now also recorded with reason `"extractor crashed (...)"` instead of disappearing silently. This bounds the worst case for any future grammar pathology — `tracedecay sync` can no longer hang forever on a single malformed file.

## [4.3.12] - 2026-05-09

### Changed
- **The beta channel is open again** — `tracedecay channel beta` was hard-gated to `"the beta channel is not available at this time"` while the prior 4.5.x beta line was being merged into stable. With v5.0.0-beta.1 published on the prerelease channel, the gate is removed: `switch_channel` now resolves `"beta"` through the same path as `"stable"` and downloads the latest GitHub prerelease. The `unknown channel` error message also lists `beta` as a valid target again.
- **Retired the "beta channel has been merged into stable" nudge** in `main.rs`. Beta users (anyone whose binary version contains `-`) used to see the nudge on every invocation; with the channel reopened the nudge is no longer correct. Beta users now stay on beta until they explicitly run `tracedecay channel stable`.

### Fixed
- **`tracedecay wipe` no longer leaks the global DB into the wipe set when `$HOME` is symlinked** — the home `.tracedecay` skip relied on lexical path equality, so a user whose `$HOME` resolves through a symlink (e.g. macOS `/Users/x` vs the canonical `/private/var/...`) could see `~/.tracedecay` show up as a wipe target if the descendant walk reached it via the canonical chain. The skip now canonicalizes both the home path and every candidate before comparing.

### Changed (carried forward from the prior unreleased section)
- **Descendant walk for `tracedecay wipe` / `tracedecay list` is now iterative with cycle protection** — `find_descendant_tracedecay` used to recurse, which made deep trees a stack-overflow risk and relied entirely on `file_type()` skipping symlinks for cycle safety. It now uses an explicit worklist plus a canonical-path `visited` set, so the walk is bounded even if a directory cycle slips past the symlink filter (e.g. Windows junctions).
- **`tracedecay doctor` purges stale global-DB entries in batched statements** — purging used to issue one `DELETE` per stale row, which meant N serial round-trips against libsql for a stale-store cleanup (the case that prompted this: 216 deletes). A new `GlobalDb::delete_projects(&[String])` issues one `DELETE … WHERE path IN (…)` per chunk of 256, so the same 216-row purge is now one round-trip.
- **`gather_local_projects_from` is now a separately-exported helper** — extracts the pure discovery logic from the cwd-driven `gather_local_projects` wrapper so the ancestor + descendant walk can be unit-tested without mutating the process's working directory. Backed by 7 new tests covering cwd / ancestor-only / descendant-only / ancestor+descendant dedup / `node_modules` skip / canonical home-skip / empty-dir.
- **Cleared `clippy::map_unwrap_or` warning in `display::shuffle_flags`** — the xorshift seed now uses `map_or` instead of `map(...).unwrap_or(...)`. Behavior unchanged.

## [4.3.11] - 2026-05-09

### Added
- **`tracedecay doctor` now reports stale entries in the global DB and offers to purge them** — projects registered in `~/.tracedecay/global.db` whose `.tracedecay/` directory is gone (deleted, moved, or scratch dirs cleaned up by the OS) are listed under the "Global database" section. Up to 10 paths are shown with an "… and N more" tail. When run interactively, the doctor prompts `Purge N stale row(s) from the global DB? [Y/n]`; on confirmation each stale row is deleted via `GlobalDb::delete_project`. When stdin is not a terminal (CI, piped invocation), the stale list is shown as a warning with a hint to re-run interactively.

### Fixed
- **`tracedecay reinstall` now refreshes every detected agent, not just the first one ever installed** — `migrate_installed_agents` previously returned early as soon as `installed_agents` was non-empty. A user who installed agent A and later configured agent B (e.g. installed Copilot first, then Claude) would have only A in the list, so `reinstall` silently skipped B and its tool permissions never got refreshed when new tools shipped. The migration now scans every agent on each call and additively appends any whose tracedecay config exists on disk but is missing from the tracked list. Side effect: a stale `tracedecay install` warning ("N new tracedecay tool(s) not yet permitted") could persist across reinstalls — that no longer happens. The detection logic is also extracted into a pure `detect_missing_installed_agents` helper covered by a regression test that reproduces the original "claude missing when copilot is tracked" scenario.
- **`tracedecay wipe` warning banner now reaches full width** — the colored title row was 49 visual columns while the `═` rules above and below were 64, producing a short red strip floating between long horizontal lines. The title is now centered and padded with red-background spaces, sandwiched between two blank red rows so the warning reads as a single fixed-width block.

## [4.3.10] - 2026-05-09

### Added
- **`tracedecay list` command for inspecting tracked projects** — `list` shows the same projects `wipe` would target (current folder, ancestors, and descendants), with on-disk `.tracedecay/` size and tokens-saved per row, sorted by tokens-saved descending. `tracedecay list --all` (or `-a`) lists every project tracked in `~/.tracedecay/global.db`, marking entries whose `.tracedecay/` directory has been removed as `(stale)`.

### Changed
- **Country flags in `tracedecay status` are now shuffled on every render** — when more flags are tracked than fit on the line, the row used to always show the same prefix and `…` truncate the rest. Each `status` invocation now applies a Fisher-Yates shuffle (xorshift64 seeded from time + PID) before truncation, so a different sample of contributing countries is shown each time.

### Fixed
- **Tool-permission warning now points at `tracedecay reinstall`** — when new tracedecay tools are detected that aren't yet permitted in the agent config, the warning previously said "Run `tracedecay install` to update", which would re-do the full install. The warning now reads "Run `tracedecay reinstall` to update permissions", which is the right command for refreshing permissions on already-installed agents.

## [4.3.9] - 2026-05-09

### Added
- **`tracedecay wipe` command for clearing local DBs** — `wipe` finds every `.tracedecay/tracedecay.db` project in the current folder, all its ancestors, and all its descendants (skipping `node_modules`, `target`, `.git`, `vendor`, `dist`, `build`, `.next`, `.venv`, `__pycache__`, and the user-level `~/.tracedecay/`), then prompts for a `go!` confirmation before removing each `.tracedecay/` directory and its row in the global DB. `tracedecay wipe --all` (or `-a`) instead wipes every project tracked in `~/.tracedecay/global.db` and then deletes the global DB itself, leaving it empty. Both flows display a bordered, blinking warning that lists every target before asking for confirmation.

## [4.3.8] - 2026-05-06

### Added
- **`DISABLE_TRACEDECAY=true` environment variable to opt out per-project (#19)** — when set in the MCP server configuration, the `serve` command exits cleanly without initializing. This lets users selectively disable tracedecay for large projects that consume too much RAM, without removing it from their global agent config.

## [4.3.7] - 2026-05-06

### Fixed
- **Incremental sync no longer aborts on cross-file edge references (#58)** — `insert_edges` now uses a conditional INSERT that silently skips edges whose source or target node does not yet exist in the database. Additionally, both incremental sync loops now insert all nodes first and queue edges for a second pass, so cross-file edges within the same sync batch always find their targets. Previously, `INSERT OR IGNORE` did not suppress FK violations, causing the sync to abort with `FOREIGN KEY constraint failed`.

## [4.3.6] - 2026-05-06

### Fixed
- **`upgrade` no longer stops the daemon when release assets aren't ready yet** — the preflight asset check now runs before stopping the daemon, so if CI hasn't finished building the release binaries, the command exits cleanly without disrupting the running MCP server.

## [4.3.5] - 2026-05-06

### Changed
- **Copilot MCP server now passes the workspace folder to `serve`** — both the VS Code (`mcp.servers.tracedecay`) and the Copilot CLI (`mcpServers.tracedecay`) registrations now launch the daemon as `tracedecay serve -p ${workspaceFolder}` instead of plain `tracedecay serve`. This lets the MCP server scope its index to the active workspace automatically without requiring a manual `-p` flag.
- **Copilot agent args validation tightened** — tests for `CopilotIntegration` now verify that `"serve"` is strictly the first argument and that all remaining args are limited to `-p` / `${workspaceFolder}`. This prevents silent regressions where extra or reordered flags could be injected into the MCP server launch command.

### Fixed
- **`serve` now falls back to the global project database when CWD discovery fails (#55)** — when VS Code Copilot (or another host) launches `tracedecay serve` with the working directory set to the user's home folder and `${workspaceFolder}` fails to resolve, the server now checks `~/.tracedecay/global.db` for registered projects. If exactly one project is found, it is used automatically; if multiple are found, they are listed on stderr with guidance to pass `-p <path>`.
- **`insert_at` no longer strips the trailing newline from edited files (#57)** — `str::lines()` discards the final `\n`, so the file was silently rewritten without its POSIX-required trailing newline. The join result now re-appends `\n` when the original file ended with one.
- **Clippy CI failures resolved** — fixed 6 `deny`-level clippy errors across extractors (identical `if`/`else` blocks in clojure, redundant `trim()` before `split_whitespace` in haskell, `map_or` → `is_some_and`, `Iterator::last` → `next_back` in SQL, `too_many_arguments` allow in haskell `emit`).
- **Foreign-key violations during incremental sync now point at the recovery path** — when an extractor produces an edge whose source or target is not in the same file's node set, `tracedecay sync` would die with `failed to insert edge: SQLite failure: FOREIGN KEY constraint failed` and no guidance. Full re-index masks this because bulk load disables FK enforcement, so the top-level error handler now detects this specific failure and suggests `tracedecay sync -f`.
- **Spinner no longer leaks on early exit** — added `Drop` for `Spinner` so when `?` propagates an error mid-sync the worker thread is joined, the line is cleared, and the cursor is restored. Previously the cursor stayed hidden after a failed sync.

## [4.3.4] - 2026-05-02

### Fixed
- **`tracedecay sync` no longer hangs on large monorepos with `node_modules` symlinks** — the directory walker now prunes excluded directories (e.g. `node_modules`, `vendor`, `build`) at the `filter_entry` level before descending into them. Previously, exclusions were only checked per-file after the walker had already entered the directory, so monorepo setups where a package manager creates symlinks inside `node_modules` pointing back into source directories (e.g. `../../api`) could cause the scanner to spin indefinitely. Closes #36.

## [4.3.3] - 2026-05-02

### Added
- **`tracedecay_body`** — new MCP tool that returns the full source body of a symbol by name (function, struct, const, etc.). Collapses search + node lookup + file read into a single call; returns multiple ranked matches when the name is ambiguous.
- **`tracedecay_todos`** — new MCP tool that finds TODO, FIXME, XXX, HACK, WIP, NOTE, and UNIMPLEMENTED markers across the project. Each result includes the marker kind, file, line, the comment text, and the enclosing symbol name. Filterable by marker kind and path prefix.

### Fixed
- **SQL (and 8 other new-language) files no longer panic during sync** — `tracedecay-large-treesitters 0.4.0` is now published to crates.io and `Cargo.toml` references the registry version instead of a local path. Users who built 4.3.2 via `cargo install` received the old 0.3.2 grammar bundle (no SQL), causing a panic per `.sql` file. Closes #53.

### Changed
- **`tracedecay-large-treesitters` dependency pinned to published 0.4.0** — switched from a local path dependency to `"0.4.0"` so `cargo install tracedecay` picks up the full grammar set including SQL, R, Julia, Haskell, OCaml, Clojure, Erlang, Elixir, and F#.

### Internal
- **Grammar completeness test** — `ts_provider::tests::all_extractor_keys_are_registered` verifies every language key an extractor passes to `ts_provider::language()` is present in the bundled grammar table. CI will catch mismatches before a release ships.

## [4.3.2] - 2026-05-01

### Added
- **9 new language extractors — R, SQL, Julia, Haskell, OCaml, Clojure, Erlang, Elixir, F#** — closes the gap between tracedecay and sentrux for functional and data-science languages. Each extractor handles the language's primary top-level constructs and is gated behind its own `lang-*` feature flag, all included in `full`:
  - **R** (`.r`, `.R`) — function assignments (`foo <- function(...)`), call sites, roxygen2 docstrings. Requires `tracedecay-large-treesitters` ≥ 0.4.0.
  - **SQL** (`.sql`) — `CREATE TABLE`, `CREATE VIEW`, `CREATE FUNCTION`, `CREATE PROCEDURE` via `tree-sitter-sequel`.
  - **Julia** (`.jl`) — `function`, `macro`, `struct`, `abstract_definition`, `module` definitions; import/using nodes.
  - **Haskell** (`.hs`, `.lhs`) — `function`/`bind` declarations, `data_type`/`newtype`, `class`, `instance`, `import` nodes.
  - **OCaml** (`.ml`, `.mli`) — top-level `let_binding` (function vs const), `type_definition`, `module_definition`, `class_definition`, `open` nodes.
  - **Clojure** (`.clj`, `.cljs`, `.cljc`) — `defn`/`defmacro`, `ns`, `def`/`defonce`, `defprotocol`/`defrecord`/`deftype` via `list_lit` dispatch on the first symbol.
  - **Erlang** (`.erl`, `.hrl`) — `fun_decl` with arity-qualified names (`foo/2`), `-module` attribute, `-type`/`-opaque` declarations.
  - **Elixir** (`.ex`, `.exs`) — `def`/`defp`, `defmodule`, `defmacro`/`defmacrop`, `defstruct` via `call`-node dispatch on the function head.
  - **F#** (`.fs`, `.fsi`, `.fsx`) — `function_or_value_defn`, `type_definition`, `module_defn`, `namespace`, `open_decl` nodes.
- **Complexity configs for all 9 new languages** — `R_COMPLEXITY`, `SQL_COMPLEXITY`, `JULIA_COMPLEXITY`, `HASKELL_COMPLEXITY`, `OCAML_COMPLEXITY`, `CLOJURE_COMPLEXITY`, `ERLANG_COMPLEXITY`, `ELIXIR_COMPLEXITY`, `FSHARP_COMPLEXITY` added to `src/extraction/complexity.rs`.
- **`tracedecay-large-treesitters` 0.4.0** — bundles the 9 new tree-sitter grammars: `tree-sitter-r`, `tree-sitter-sequel`, `tree-sitter-julia`, `tree-sitter-haskell`, `tree-sitter-ocaml`, `tree-sitter-clojure-orchard`, `tree-sitter-erlang`, `tree-sitter-elixir`, `tree-sitter-fsharp`.

### Fixed
- **`tracedecay monitor` displayed temp directories as projects** — MCP clients that create per-request temp directories (names matching `.tmp…`) were appearing as project entries in the monitor. These are now filtered out at render time; the TOTAL line reflects only real projects.

### Changed
- **`tracedecay monitor` now supports scrolling** — Up/Down arrows scroll one line at a time; PageUp/PageDown scroll one screen. Scroll offset is clamped to the available content and resets to zero on Ctrl+R. Footer hint updated accordingly.

## [4.3.1] - 2026-05-01

### Fixed
- **`tracedecay_str_replace`, `tracedecay_multi_str_replace`, and `tracedecay_insert_at` silently mutated files for unsupported types (issue #51)** — all three tools write the file to disk and then call `reindex_file` to update the graph. For file types without a registered extractor (e.g. `.css`, `.html`), `reindex_file` returned `Err("unsupported file type: …")`; the `?` propagated that error to the caller, which reported tool failure — but the write had already been committed. The fix changes `reindex_file` to return `Ok(())` early when no extractor is found, so edits to unsupported file types succeed and the graph simply skips reindexing for those files.

### Changed
- **Sync duration is now tracked and displayed** — `GraphStats` gains a `last_sync_duration_ms` field persisted to the metadata store. All three sync paths (full index, `sync_single_files`, `sync_with_progress_verbose`) write this value. The status table's sync row now shows the duration inline: `Last sync 2m ago (1.2s)  Full sync 1d ago`. Duration is omitted when the value is unknown (existing databases before this change).

## [4.3.0] - 2026-04-30

### Added
- **Subprocess-isolated extraction** — every file is now parsed inside a short-lived worker process rather than in the sync process itself. If a tree-sitter grammar segfaults, calls `abort()`, or otherwise terminates by a path Rust cannot intercept, only the worker dies; the pool respawns it, the offending file is logged and skipped, and sync continues. This is a stronger guarantee than the v4.2.1 `catch_unwind` defense, which could only catch Rust panics.
  - The worker is exposed via a hidden subcommand (`tracedecay extract-worker`) that authenticates against the parent through a 256-bit per-spawn token: required as both an env var and as the first 32 bytes on stdin. A user invoking the binary directly hits the missing-env check and exits non-zero. The subcommand is also hidden from `--help`.
  - When `current_exe()` does not point at a real `tracedecay` binary (e.g. under `cargo test`, where the test harness is the running binary), extraction transparently falls back to the in-process path. Tests therefore continue to exercise extractors directly without needing to spawn subprocesses.
  - Defaults to `available_parallelism()` workers; opt out via `TRACEDECAY_DISABLE_SUBPROCESS=1` if needed.

### Changed
- Single-file extraction (used by the `tracedecay_str_replace`, `tracedecay_insert_at`, etc. edit tools) still runs in-process — the subprocess overhead is unjustified for one-shot operations and these tools are interactive enough that an extractor crash is immediately visible.

## [4.2.1] - 2026-04-30

### Fixed
- **Sync no longer aborts when a tree-sitter grammar hits an internal assertion (issue #49)** — the vendored `tree-sitter-markdown` C++ scanner contains `assert()` calls that, on certain autolink constructs, called `abort()` and killed the entire `tracedecay sync` process (core-dumped on Linux). Two layers of defense:
  - Added `.cargo/config.toml` with `CFLAGS=-DNDEBUG` and `CXXFLAGS=-DNDEBUG`. `cc-rs` reads these env vars when compiling vendored grammars in `tracedecay-large-treesitters`'s build script, disabling C/C++ assertions in release builds. A failed assertion now degrades to a malformed parse tree (which the extractor handles gracefully) instead of `SIGABRT`.
  - Added a `safe_extract` helper that wraps every `extractor.extract()` call site with `std::panic::catch_unwind`. A Rust panic from any extractor (malformed input, future bugs) now logs the file path and skips it instead of bringing down the whole sync.
- See issue #50 for the broader follow-up: migrating to pure-Rust generated parsers via the `--rust` fork of tree-sitter to eliminate this class of failure entirely.

## [4.2.0] - 2026-04-30

### Added
- **Health & structural analysis tools** — seven new MCP tools that expose quality insights from the existing code graph:
  - `tracedecay_health` — composite quality signal (0–10000) from five independent dimensions: acyclicity, depth, equality, redundancy, and modularity. Uses geometric mean so no single dimension can be gamed. Supports `details: true` for per-dimension breakdown.
  - `tracedecay_gini` — Gini inequality coefficient for any metric (complexity, lines, fan_in, fan_out, members) across files or symbols. Identifies god files and uneven complexity distribution with interpretive labels and ranked outliers.
  - `tracedecay_dependency_depth` — longest file-level dependency chains (Lakos levelization). Shows transitive fragility that direct coupling metrics miss, with full chain reconstruction after cycle-breaking via Tarjan's SCC.
  - `tracedecay_dsm` — Design Structure Matrix in three output formats: `stats` (density, cluster count), `clusters` (per-directory edge analysis), and `matrix` (NxN grid with short filenames). Reveals hidden coupling patterns and layering violations.
  - `tracedecay_test_risk` — risk-weighted test gap analysis combining complexity, fan-in, test coverage, and git churn (90-day window) into a single score. Answers "where should the next test go?" with `include_tested` option for finding weak-test candidates.
  - `tracedecay_session_start` — saves current health metrics as a JSON baseline for later comparison. Call before starting an AI coding session.
  - `tracedecay_session_end` — re-computes health and diffs against the session baseline. Reports per-dimension deltas with improved/degraded/unchanged labels, overall pass/fail, and cleans up the baseline file.
- **Git churn integration** — new `src/graph/git.rs` module shells out to `git log` at runtime to compute per-file commit frequency. Used by `tracedecay_test_risk` as a risk multiplier (log2-scaled) without persisting any data to the tracedecay DB.
- **File-level DAG builder** — new `build_file_adjacency` method on `GraphQueryManager` constructs a directed file dependency graph from the existing edge data in a single SQL query. Shared foundation for health, depth, DSM, and modularity computations.

## [4.1.8] - 2026-04-30

### Added
- **`include` config glob** — new `include` field in `.tracedecay/config.json` lets users whitelist hidden (dot-prefixed) paths for indexing. By default, all dot-directories are skipped during sync; paths matching an `include` glob (e.g. `[".github/**"]`) are now walked and indexed. The `exclude` list still applies after inclusion, so `.git/**` and `.tracedecay/**` remain filtered even with broad include patterns.
- **Markdown extraction** — tree-sitter based markdown parser that extracts headers as `Module` nodes with hierarchical `Contains` edges, and code links as `Uses` edges for cross-reference tracking (PR #47)

## [4.1.7] - 2026-04-29

### Fixed
- **Nested `.gitignore` files were silently ignored** — `git_ignore(true)` in the `ignore` crate relies on git repository detection (walking up to find `.git`) to build the gitignore rule stack. When the walk root was outside a git repo — or in a subdirectory that the crate couldn't trace back to a `.git` — rules in nested `.gitignore` files were never applied. Added `add_custom_ignore_filename(".gitignore")` to the `WalkBuilder`, which makes the crate read every `.gitignore` it encounters as a standalone ignore source regardless of git repo presence. Five regression tests cover: subdirectory exclusion, scope isolation, negation overrides, deep descendant exclusion, and a direct `ignore`-crate sanity check.

## [4.1.6] - 2026-04-29

### Fixed
- **`logging/setLevel` returned MethodNotFound on every session start** — the server correctly advertised the `logging` capability in its `initialize` response (required for the `notifications/message` version-warning feature), but had no handler for the `logging/setLevel` request that MCP clients send immediately after. Every session produced a `-32601` error in the client log. The handler now returns an empty success as required by the MCP spec (RFC 5424 log-level filtering is advisory; the server continues to emit notifications at its own discretion).
- **`java_extraction` panic on empty Javadoc** — parsing a Java file containing a docstring with no content caused a panic (fixes #44).

## [4.1.5] - 2026-04-29

### Added
- **Edit primitives for code modification** — four new MCP tools enable Claude and friends to edit files without regex or shell quoting hazards (PR #43 by @pierreaubert):
  - `tracedecay_str_replace` — replaces a unique `old_str` with `new_str`; fails if 0 or >1 matches, protecting against multi-edit bugs
  - `tracedecay_multi_str_replace` — applies N `(old, new)` replacements atomically; all-or-nothing transaction
  - `tracedecay_insert_at` — inserts content before or after a unique anchor string or line number
  - `tracedecay_ast_grep_rewrite` — structural code rewrite via ast-grep CLI (`--rewrite` mode)
- **Auto re-indexing** — all four edit tools automatically re-index the modified file in the code graph after writing, keeping the graph in sync without manual steps (PR #43 by @pierreaubert)

### Performance
- **Fixed N+1 query patterns in graph traversal** — `traverse_bfs`, `traverse_dfs`, `get_callers`, `get_callees`, `get_file_dependencies`, `get_file_dependents`, and `find_dead_code` were each making a separate database query per node, causing excessive CPU usage on large codebases. All methods now batch-fetch nodes using a single `WHERE id IN (...)` query, reducing database roundtrips from O(N) to O(1). (PR #40 by @pierreaubert)

### Fixed
- **`find_dead_code` hit SQLite variable limit on large codebases** — the query used `IN (?, ?, …)` binds which SQLite caps at 999 variables; replaced with `NOT EXISTS (SELECT 1 FROM edges WHERE …)` to avoid the limit entirely. (PR #43 by @pierreaubert)
- **`tracedecay_test_map` failed to resolve cross-crate qualified calls** — when a reference contained `::` (e.g. `crate_name::func`), a failed qualified-name match returned `None` without falling back to a simple-name lookup, breaking test coverage queries for integration tests that call across crate boundaries. Fixed by removing the early return and adding a simple-name fallback that strips the qualifier before matching. (PR #43 by @pierreaubert)
- **Sync frequency reduced and stale-warning auto-sync added** — sync interval dropped from its previous default to 2 s (configurable); the MCP server now automatically triggers a live sync when an agent receives a stale-graph warning, avoiding a manual `tracedecay sync` round-trip. (PR #43 by @pierreaubert)
- **`TOOL_NAMES` and `EXPECTED_TOOL_PERMS` were static** — `doctor` and `install` would not detect or register newly-introduced MCP tools. Both lists are now built dynamically so adding a tool automatically propagates to health checks and permission installation. (PR #43 by @pierreaubert)
- **`tracedecay monitor` now groups output per project then per tool** — previously all tool calls were listed in a flat stream; entries are now grouped by project path first, then by tool name, making it easier to see which project is driving activity. (PR #43 by @pierreaubert)

## [4.1.4] - 2026-04-25

### Fixed
- **`tracedecay monitor` panicked on macOS/Linux with "Cannot start a runtime from within a runtime" (issue #39)** — the previous fix for the Windows panic kept a Unix-only branch that built a new `tokio::runtime` and called `block_on` from inside `#[tokio::main]`, which panics on every platform, not just Windows. `refresh_cost_cache` now uses `block_in_place + Handle::current().block_on` unconditionally, since `monitor::run()` is always invoked from the existing multi-threaded runtime.

## [4.1.3] - 2026-04-24

### Fixed
- **Backslashed Windows hook paths never self-healed (issue #38)** — the v4.0.2 fix for #20 normalized `which_tracedecay()` output but could not rewrite existing settings. `install_single_hook` is idempotent by presence, so when a tracedecay hook already existed with a backslashed path, the silent backfill in `check_install_stale` left it untouched. Additionally, the backfill only scanned `~/.claude/settings.json` — project-level `.claude/settings.json` and `.claude/settings.local.json` were never touched, so opening a previously-configured project could still trigger `bash: C:Usersalkamscoopappstracedecaycurrenttracedecay.exe: command not found`. Fixed with a new `normalize_hook_command_paths` pass that rewrites any backslash-containing tracedecay hook command to forward slashes, and by extending the backfill to the current project's `.claude` directory.

## [4.1.2] - 2026-04-22

### Added
- **Mistral Vibe agent integration** — `tracedecay install --agent vibe` registers the tracedecay MCP server in Vibe's `~/.vibe/config.toml` as a `[[mcp_servers]]` stdio entry, and appends prompt rules to `~/.vibe/prompts/cli.md`. Supports install, uninstall, and healthcheck. Respects the `VIBE_HOME` environment variable. Closes #37.

## [4.1.1] - 2026-04-22

### Added
- **`tracedecay sync --verbose` (`-v`)** — prints per-phase diagnostic lines during sync to help diagnose slow or stuck syncs on large repos. Shows file counts, change breakdowns, and timings for each phase (scan, stat-check, hash, content check, index, resolve, DB write). Also works with `--force` full re-index. Addresses #36.

## [4.1.0] - 2026-04-20

### Added
- **Walk-up project discovery** — `tracedecay serve`, `tracedecay sync`, and `tracedecay status` now walk up the directory tree to find the nearest `.tracedecay/` database when no `--path` is given. This means you can launch an AI agent from a subdirectory of your project and tracedecay will find the index automatically — similar to how git finds `.git/`. `tracedecay init` is unchanged and always creates a new project at the target directory.
- **Subdirectory scope filtering** — when the MCP server is started from a subdirectory, listing and discovery tools (`tracedecay_files`, `tracedecay_search`, `tracedecay_context`, `tracedecay_dead_code`, `tracedecay_rank`, `tracedecay_largest`, `tracedecay_coupling`, `tracedecay_complexity`, `tracedecay_doc_coverage`, `tracedecay_god_class`, `tracedecay_unused_imports`, `tracedecay_hotspots`, and others) automatically scope results to that subdirectory. Graph traversal tools (`tracedecay_callers`, `tracedecay_callees`, `tracedecay_impact`, `tracedecay_affected`, `tracedecay_type_hierarchy`) remain unscoped so cross-directory relationships are preserved. The user can always override the scope by providing an explicit `path` parameter. `tracedecay_status` reports the active scope prefix when one is in effect.

## [4.0.7] - 2026-04-18

### Fixed
- **Symlinked source directories were not indexed** — both the plain `walkdir` and `.gitignore`-aware `ignore::WalkBuilder` file discovery paths now follow symlinks (`follow_links(true)`), so projects that expose source code through symlinked directories are fully indexed. (PR #34 by @lesbass)

## [4.0.6] - 2026-04-18

### Added
- **GLSL language support** — new tree-sitter-based extractor for OpenGL shading language files (`.glsl`, `.vert`, `.frag`, `.geom`, `.comp`, `.tesc`, `.tese`). Extracts functions, structs with fields, uniform/in/out/varying declarations, preprocessor defines, call sites, and complexity metrics. Requires `tracedecay-large-treesitters` 0.3.0. Feature-gated as `lang-glsl` in the Full tier. Closes #35.

### Fixed
- **`tracedecay upgrade` fails on Homebrew installs** — `self_replace` failed with `ENOENT` on Homebrew symlinks because it resolved relative symlink targets from CWD instead of the symlink's parent. Now dispatches to install-method-aware replacement: Homebrew bypasses `self_replace` and atomically replaces the binary at the canonical Cellar path, renames the version directory, and updates the symlink + `INSTALL_RECEIPT.json` so `brew` reports the correct version. Scoop updates the version directory, junction, and `manifest.json`. Other symlinked installs get a canonicalization fallback. Supersedes PR #33.

## [4.0.5] - 2026-04-17

### Changed
- **Separate `tracedecay init` from `tracedecay sync`** — previously, `tracedecay sync` silently created a new database if none existed. This was a problem because the global git post-commit hook runs `tracedecay sync` in every repo after each commit, causing phantom `.tracedecay/` databases to appear in projects that never opted in. Now `tracedecay init` handles first-time project setup (creates DB + full index) and errors if already initialized, while `tracedecay sync` only performs incremental updates and errors if the project was never initialized. The git hook (`tracedecay sync >/dev/null 2>&1 &`) now safely exits with an error in non-enrolled repos — no database created. All agent setup messages and documentation updated to reference `tracedecay init` for first-time use.

## [4.0.4] - 2026-04-17

### Added
- **Google Antigravity support** — new `tracedecay install --agent antigravity` registers the MCP server in `~/.gemini/antigravity/mcp_config.json`. Includes install, uninstall, healthcheck, and auto-detection. Closes #24.
- **Kilo CLI support** — new `tracedecay install --agent kilo` registers the MCP server in `~/.config/kilo/kilo.jsonc` using Kilo's `mcp` key with `type: "local"` format. Includes install, uninstall, healthcheck, and auto-detection. Closes #31.

### Changed
- **Simpler install prompts** — `tracedecay install` now asks a Y/n question per detected agent instead of showing a multi-select dialog box. Prints a +/- summary of changes at the end. Removed `dialoguer` dependency.
- **No-op upgrade is no longer an error** — `tracedecay upgrade` when already on the latest version now exits successfully instead of printing a misleading error. Same for `tracedecay channel` when already on the requested channel. (PR #30 by @lesbass)

### Fixed
- **Default branch detection wrote `"HEAD"` instead of actual branch name** — `detect_default_branch()` used `reference.name()` on the `refs/remotes/origin/HEAD` symbolic ref, which returns the ref's own name. Now resolves through `reference.follow()` to get the target (e.g. `refs/remotes/origin/master`), then strips the prefix correctly. (PR #26 by @LucioPg)
- **Branch detection in git worktrees** — `current_branch()` read `.git/HEAD` directly as a plain file, which fails in git worktrees where `.git` is a pointer file (not a directory). Fixed with a two-tier approach: `gix::open()` first, then `git symbolic-ref -q HEAD` subprocess fallback. (PR #28 by @LucioPg)
- **Windows monitor nested runtime panic** — `tracedecay monitor` cost cache refresh panicked on Windows due to nested tokio runtimes. Now uses `block_in_place` + `Handle::current()` on Windows. (PR #29 by @LucioPg)
- **Clippy clean** — resolved all clippy errors across the codebase; CI clippy step now passes.

## [4.0.3] - 2026-04-16

### Fixed
- **Windows daemon nested runtime panic** — `tracedecay daemon` panicked on Windows because `daemon-kit` runs the closure inline (no fork), creating a nested tokio runtime. Now uses `block_in_place` + `Handle::current()` on Windows while keeping `Runtime::new()` on Unix where the forked child genuinely has no runtime.

## [4.0.2] - 2026-04-14

### Added
- **Token cost observability** — new `tracedecay cost` command parses Claude Code session transcripts (`~/.claude/projects/**/*.jsonl`), classifies each API turn into 13 task categories (coding, debugging, exploration, ...), and computes dollar cost per model. Supports `--by-model`, `--by-task`, `--export json|csv`, and time ranges (`today`, `7d`, `30d`, `all`). Model pricing is refreshed from LiteLLM every 24 hours and cached at `~/.tracedecay/pricing.json`. Cost data is stored in the existing `~/.tracedecay/global.db`. The `tracedecay status` header now shows today's cost, 7-day cost, and efficiency ratio. The `tracedecay monitor` TUI includes a cost panel. The `hook_stop` handler prints a session cost receipt. Task classification adapted from [AgentSeal/codeburn](https://github.com/AgentSeal/codeburn).
- **`tracedecay status --details`** — the node-kind breakdown table is now opt-in via the `--details` flag. Default status output is more compact.
- **Per-file diversity caps** — `tracedecay_context` now limits how many symbols from a single file appear in results (default: `max_nodes/3`, minimum 3), preventing one large file from dominating context output. Configurable via the new `max_per_file` parameter.
- **Exact name match supplementing** — context search now supplements FTS5 results with exact case-insensitive name lookups, so perfect symbol name matches are never buried by BM25 noise.
- **Stem variant search expansion** — search terms are expanded with suffix-based stem variants (e.g. "authenticate" also finds "authentication", "authenticator") via 13 derivational suffix rules, improving recall for conceptual queries.
- **Co-occurrence boosting** — when a query has multiple terms, symbols where 2+ terms co-locate in name, qualified name, or file path get a multiplicative score boost, improving precision on multi-word searches.
- **Edge recovery after node trimming** — when BFS subgraph expansion trims nodes to fit `max_nodes`, edges are now filtered to retain only those connecting surviving nodes, keeping the returned subgraph consistent.
- **Adaptive SQLite pragmas** — `cache_size` and `mmap_size` now scale to the DB file size instead of using fixed 64 MB / 256 MB values. Small projects (5 MB DB) drop from ~320 MB baseline to ~12 MB; large projects keep the same performance.
- **`tracedecay reinstall` command** — re-runs install for all already-configured agents, refreshing MCP server registration, hooks, permissions, and prompt rules without the interactive picker.

### Removed
- **Graph visualizer** — `tracedecay visualize` command, `src/visualizer.rs`, and the embedded HTML file have been removed. The upstream CodeGraph project also removed its visualizer in the same period.

### Fixed
- **Windows path separators in hooks and MCP config** — `which_tracedecay()` now normalizes backslash paths to forward slashes, fixing broken hook command execution on Windows (e.g. Scoop installs). Existing settings with backslash paths are also normalized when read back.

## [4.0.0] - 2026-04-13

### Added
- **Multi-branch indexing** — opt-in per-branch databases so switching branches never gives stale results. `tracedecay branch add` tracks a branch by copying the nearest ancestor DB and syncing only changed files. `tracedecay branch list`, `tracedecay branch remove`, `tracedecay branch removeall`, and `tracedecay branch gc` manage tracked branches.
- **`tracedecay branch removeall`** — remove all tracked branches except the default in one command, deleting their DB files.
- **`tracedecay_branch_search`** MCP tool — search symbols in another branch's code graph without switching your checkout.
- **`tracedecay_branch_diff`** MCP tool — compare code graphs between two branches: shows symbols added, removed, and changed (signature differs). Supports file and kind filters.
- **`tracedecay_branch_list`** MCP tool and **`tracedecay://branches`** MCP resource — list tracked branches with DB sizes, parent branch, sync times.
- **Branch fallback warnings** — when the MCP server serves from an ancestor branch DB (current branch not tracked), every tool response warns to `tracedecay branch add`.
- **`keywords` parameter for `tracedecay_context`** — agent-driven synonym expansion. Pass extra search terms (e.g. `["login", "session", "token"]` for "authentication") and the context builder searches each keyword independently, bridging conceptual queries to lexically-unrelated symbol names without embedding models.
- **`tracedecay monitor` CLI command** — global live TUI showing MCP tool calls from all projects in real time via a shared memory-mapped ring buffer at `~/.tracedecay/monitor.mmap`. Entries show `prefix - project - tool_name` so multiple tool suites and projects are distinguishable. Uses `memmap2` with file locking for concurrent writer safety.
- **`path` filter on 7 analytics MCP tools** — `tracedecay_god_class`, `tracedecay_largest`, `tracedecay_complexity`, `tracedecay_rank`, `tracedecay_coupling`, `tracedecay_inheritance_depth`, and `tracedecay_recursion` now accept an optional `path` parameter to scope results to a directory (e.g. `"path": "src/main/java"`), preventing large languages from dominating global rankings.
- **Right-click context menu in graph visualizer** — callers, callees, call graph, and impact actions on node right-click.
- **Type annotation references** — TypeScript, Java, and Kotlin type annotation references now tracked as edges in the graph.
- **Graph visualizer** — interactive Cytoscape.js-based code graph visualization served via `tracedecay visualize`.
- **Daemon version mismatch detection** — `tracedecay daemon --status` warns when the daemon version differs from the CLI with a corrective restart command.
- **Parent branch in status output** — `tracedecay status` and `tracedecay_status` now show which branch a tracked branch was seeded from.

### Removed
- **Vector/embedding module** — removed `src/vectors/`, `enable_embeddings` config field, and `Vector` error variant. The `keywords` parameter on `tracedecay_context` replaces the need for local embedding models. The `vectors` DB table is retained (empty, harmless) to avoid migration issues.

### Changed
- **Monitor is now global** — moved from per-project (`<project>/.tracedecay/monitor.mmap`) to machine-level (`~/.tracedecay/monitor.mmap`). `tracedecay monitor` no longer takes a `--path` flag.
- Quality improvements to resolution, search, and traversal.
- Tool count increased from 34 to 37.

### Dependencies
- Added `memmap2`, `crossterm`, `fs2` for the monitor feature.

## [3.5.1] - 2026-04-13

### Fixed
- **Doctor validates hook subcommands** — `tracedecay doctor` now checks that each hook event uses the correct tracedecay subcommand (e.g. `hook-prompt-submit` for `UserPromptSubmit`, not an invalid or mismatched command).
- **Doctor auto-repairs broken hooks** — when a hook has a wrong subcommand or is missing entirely, `tracedecay doctor` replaces it with the correct command automatically.

### Added
- **18 unit tests for Claude hook lifecycle** — install, uninstall, doctor detection, and doctor auto-repair for all three hook events.

## [3.5.0] - 2026-04-13

### Added
- **Per-call token savings reported inline** — every MCP tool response now appends a `tracedecay_metrics: before=N after=M` line showing how many raw-file tokens were avoided.
- **`UserPromptSubmit` and `Stop` hooks** — `tracedecay install` now registers three hooks (PreToolUse, UserPromptSubmit, Stop) instead of just PreToolUse. Existing installs are silently backfilled on startup.
- **`tracedecay current-counter` / `reset-counter` commands** — expose and reset a per-project local token counter, separate from the lifetime total.
- **Respect global gitignore** for `.tracedecay` warning.

### Changed
- **Hook install/uninstall generalized** — `install_hook` and `uninstall_hook` now iterate over all three hook events.
- **Sync uses mtime/size pre-filter** — skips hashing unchanged files, only reads files whose mtime or size changed since last sync.
- **Dependency upgrades** — dialoguer 0.11→0.12, notify 7→8, sha2 0.10→0.11, zip 6→8, windows-sys 0.59→0.61.

## [3.4.6] - 2026-04-07

### Fixed
- **SQLite FTS corruption from interrupted sync** — handle UTF-16 encoded files, report unreadable files during sync.

## [3.4.5] - 2026-04-07

### Added
- **`--version` / `-V` flag** to CLI.

### Fixed
- Replace `self_update` crate with direct `ureq`+`tar`+`self_replace` implementation for more reliable upgrades.

## [3.4.4] - 2026-04-07

### Fixed
- Fix `tracedecay upgrade` ENOENT error on Homebrew symlink installs.

## [3.4.3] - 2026-04-07

### Fixed
- Handle UTF-16 encoded files and report unreadable files during sync.

## [3.4.2] - 2026-04-07

### Added
- **`tracedecay channel` command** — show or switch the update channel (stable/beta).

### Fixed
- Cross-workflow Homebrew/Scoop failures on wrong release type.
- Better upgrade error messages when CI is still building.

## [3.4.1] - 2026-04-07

### Fixed
- Beta Homebrew bottle 404 — fix bottle archive naming.
- Update notices now suggest `tracedecay upgrade` instead of platform-specific commands.

## [3.4.0] - 2026-04-07

### Added
- **`tracedecay upgrade` command** — self-update the binary directly from GitHub releases. Detects the current channel, downloads the correct platform-specific archive, and replaces the running binary.
- **Annotation/attribute extraction for 7 languages** — Rust, Swift, Dart, Scala, PHP, C++, and VB.NET. All create `AnnotationUsage` nodes with `Annotates` edges. Brings annotation support to 12 of 31 languages.
- **McpTransport trait** — zero-cost abstraction for MCP server I/O, enabling in-memory test transports.
- **370+ new tests** — line coverage 71% → 84%.

## [3.3.3] - 2026-04-05

### Added
- `tracedecay sync --doctor` lists added/modified/removed files.

## [3.3.2] - 2026-04-05

### Fixed
- **Windows build failure blocking Homebrew/Scoop updates** — `SHELLEXECUTEINFOW` in `windows-sys` 0.59 requires the `Win32_System_Registry` feature flag, which was missing. This caused Windows CI builds to fail since v3.2.0, and because the release workflow used `fail-fast: true`, the failure cascaded to skip the Homebrew tap and Scoop bucket update jobs entirely. Users on Homebrew were stuck on v3.1.0. ([#12](https://github.com/ScriptedAlchemy/tracedecay/issues/12))
- **`HANDLE` type mismatch on Windows** — `windows-sys` 0.59 changed `HANDLE` from `usize` to `*mut c_void`. The UAC elevation code now uses `std::ptr::null_mut()` and `.is_null()` instead of literal `0`.
- **Release workflow resilience** — changed build matrix to `fail-fast: false` and downstream jobs (`update-homebrew`, `update-scoop`) to `if: !cancelled()`, so a single platform build failure no longer blocks formula/manifest updates for platforms that succeeded.

## [3.3.1] - 2026-04-05

### Fixed
- **Windows `is_installed()` always returned `false`** — the daemon autostart check via `daemon-kit` used a file-path probe that returns `None` on Windows, so `is_service_installed()` never detected an existing service. This caused `tracedecay install` to re-offer autostart every time. Now dispatches to the Windows SCM query that was already implemented but never wired up. (daemon-kit 0.1.4)
- **Windows `--enable-autostart` failed on reinstall** — running `tracedecay daemon --enable-autostart` twice would error with "service already exists". The installer now stops and removes the old service before re-creating, making the operation idempotent. (daemon-kit 0.1.4)

### Added
- **Upgrade-aware daemon restart** — the background daemon now snapshots its own binary's mtime and size at startup and checks every 60 seconds. When an upgrade is detected (via `brew upgrade`, `cargo install`, `scoop update`, or any package manager), the daemon flushes pending syncs, logs the event, and exits. The service manager (launchd `KeepAlive`, systemd `Restart=on-failure`, Windows SCM failure actions) automatically relaunches with the new binary. Previously the old version ran until the next reboot or manual restart.
- **Windows SCM failure recovery** — the Windows service is now configured with `ServiceFailureActions` (restart after 5s, then 10s) so the SCM relaunches the daemon after upgrade-triggered exits.
- **Daemon version logging** — the daemon startup log now includes the version (`v3.3.1 started, watching N projects`) so log readers can confirm which version is running after an upgrade restart.

### Changed
- Bumped `daemon-kit` dependency from 0.1.3 to 0.1.4.

## [3.3.0] - 2026-04-05

### Changed
- **Sync progress now matches full-index display** — `tracedecay sync` now shows `[current/total] syncing file (ETA: Ns)` with the braille spinner and path truncation, matching the progress display used during initial indexing. Previously sync only showed phase names without file counters or ETA.

### Added
- **MCP tool annotations** — all 34 tools now include `readOnlyHint: true` and a human-friendly `title` in their MCP annotations. Clients that support annotations can run all tracedecay tools concurrently without permission prompts and display cleaner tool names.
- **`_meta["anthropic/alwaysLoad"]`** on core tools — `tracedecay_context`, `tracedecay_search`, and `tracedecay_status` are marked for immediate loading, bypassing the client's tool-search round-trip on first use.
- **Server instructions** — the MCP `initialize` response now includes an `instructions` field guiding the model to start with `tracedecay_context` and noting all tools are read-only and safe to call in parallel.
- **MCP resources** — three resources exposed via `resources/list` and `resources/read`:
  - `tracedecay://status` — graph statistics as JSON
  - `tracedecay://files` — indexed file tree grouped by directory
  - `tracedecay://overview` — project summary with language distribution and symbol kinds
- **`tracedecay_commit_context`** — semantic summary of uncommitted changes for commit message drafting. Returns changed symbols grouped by file role (source/test/config/docs), a suggested commit category, and recent commit subjects for style matching.
- **`tracedecay_pr_context`** — semantic diff between two git refs for pull request descriptions. Returns commit log, symbols added/modified, affected tests, and impacted modules.
- **`tracedecay_simplify_scan`** — quality analysis of changed files: detects symbol duplications, dead code introductions, complexity hotspots, and high-coupling files.
- **`tracedecay_test_map`** — source-to-test mapping at the symbol level. Shows which test functions call which source functions and identifies uncovered symbols.
- **`tracedecay_type_hierarchy`** — recursive type hierarchy tree for traits, interfaces, and classes showing all implementors and extenders with file locations.
- **`tracedecay_context` extended** — new `include_code` parameter includes source code snippets for key symbols (wires through to the existing context builder). New `mode: "plan"` parameter appends extension points (public traits/interfaces with implementor counts) and test coverage for related modules.

### Changed
- Tool count increased from 29 to 34.
- Trimmed verbose tool descriptions for lower token overhead in deferred tool lists (`tracedecay_rank`, `tracedecay_coupling`, `tracedecay_port_status`, `tracedecay_port_order`, `tracedecay_affected`, `tracedecay_complexity`, `tracedecay_doc_coverage`, `tracedecay_god_class`, `tracedecay_recursion`, `tracedecay_inheritance_depth`, `tracedecay_distribution`).

## [3.2.2] - 2026-04-05

### Fixed
- **MCP tools no longer warn on patch-only updates** — the `tracedecay_status` MCP tool now uses `is_newer_minor_version` instead of `is_newer_version`, so patch-level releases (e.g. 3.2.0 → 3.2.1) no longer trigger update warnings in MCP tool output. The CLI status command continues to show all available updates.
- **Separate beta/stable update channels** — `is_newer_version` now returns `false` for cross-channel comparisons (beta vs stable). Previously a beta user could be told to upgrade to a stable release, or vice versa. Each channel now only sees updates from its own channel.

## [3.1.1] - 2026-04-02

### Fixed
- **Windows daemon service installation** — `tracedecay install` and `tracedecay daemon --enable-autostart` no longer fail on non-elevated Windows terminals. When administrator privileges are required to register the Windows Service, the process now automatically requests UAC elevation for just the service installation step; everything else continues non-elevated. ([#7](https://github.com/ScriptedAlchemy/tracedecay/issues/7))
- **Quieter version update warnings** — the CLI no longer warns about patch-only releases (e.g. 3.2.0 → 3.2.1); warnings now appear only for minor or major version bumps. The status page (`tracedecay_status` MCP tool) continues to show all available updates.

## [3.1.0] - 2026-04-01

### Fixed
- **Edge duplication during incremental sync** — reference resolution was re-resolving ALL unresolved refs on every sync (not just from changed files) and inserting duplicate edges with no deduplication. Over many syncs this caused unbounded DB growth (e.g. 5.1 GB for a 108 MB codebase). A unique index on edges and `INSERT OR IGNORE` now prevent duplicates entirely. A V5 migration automatically deduplicates existing databases on upgrade. ([#5](https://github.com/ScriptedAlchemy/tracedecay/issues/5))

### Added
- **Concurrent sync prevention** — a PID-based lockfile (`.tracedecay/sync.lock`) prevents the CLI and the background daemon from running sync simultaneously. If a sync is already in progress, the second attempt fails immediately with a clear error message. Stale locks from crashed processes are reclaimed automatically.
- **`doctor` database compaction** — `tracedecay doctor` now opens the project database, reports its size, and runs `VACUUM + ANALYZE` to reclaim space. Particularly useful after upgrading from versions affected by edge duplication.
- **Index design documentation** — new `docs/INDEX-DESIGN.md` describes the full indexing pipeline, database schema, extraction process, reference resolution, incremental sync, and how `diff_context` uses the graph.

## [3.0.1] - 2026-04-01

### Fixed
- **Safe JSON config editing** — `tracedecay install` no longer silently destroys agent config files (e.g. `opencode.json`, `settings.json`) when they contain invalid or unparseable JSON. Previously, a parse failure caused the file to be silently replaced with an empty object plus the tracedecay entry, wiping all existing configuration.

### Added
- **Atomic backup before config writes** — a `.bak` copy of the original file is created (via atomic staging) before any modification. If the install fails at any point, the original file is untouched and the backup is preserved.
- **Strict JSON/JSONC loading for edits** — new `load_json_file_strict` and `load_jsonc_file_strict` functions return an error (with a helpful hint) when an existing file cannot be parsed, instead of silently returning `{}`.
- **Atomic config writes** — new content is written to a `.new` sibling file first, then atomically renamed into place via `rename(2)`. The original file is never opened for writing, so a crash or interruption cannot leave it half-written.
- **20 regression tests** covering backup creation, strict loading, atomic writes, round-trip validation, and the end-to-end install cycle for both valid and corrupt config files.

## [3.0.0] - 2026-03-28

### Changed
- **Bundled tree-sitter grammars** — all 31 language grammars now come from the `tracedecay-large-treesitters` crate (which includes `tracedecay-medium-treesitters` and `tracedecay-lite-treesitters`). Zero individual `tree-sitter-*` crate dependencies remain in tracedecay itself. The grammar provider (`ts_provider`) is a single `LazyLock<HashMap>` lookup, replacing 100+ lines of per-crate match arms.
- **Removed vendored C grammars** — the Protobuf and COBOL grammars previously compiled from C source via `build.rs` are now vendored inside the bundled crate. tracedecay no longer needs `cc` as a build dependency.
- **Simplified feature flags** — the `lang-*` feature flags still control which extractors are compiled, but no longer pull in individual grammar crate dependencies (all grammars are always present via the bundle). The `ts-ffi`/`ts-rust`/`ts-both` grammar source selection flags have been removed.

### Added
- **Daemon install prompt** — `tracedecay install` now offers to install the background daemon as an autostart service (launchd on macOS, systemd on Linux) after agent configuration. Skips silently in non-interactive mode or when the service is already installed.
- **Last sync / Full sync in status** — the status table header now shows a third row with relative timestamps for the most recent incremental sync and the most recent full reindex, stored in the metadata table.

## [2.4.0] - 2026-03-27

### Added
- **Daemon mode** — `tracedecay daemon` watches all tracked projects for file changes and runs incremental syncs automatically; debounce configurable via `daemon_debounce` in `~/.tracedecay/config.toml` (default `"15s"`)
- **Daemon management** — `--stop`, `--status`, `--foreground` flags for process control; PID file at `~/.tracedecay/daemon.pid`
- **Autostart service** — `--enable-autostart` / `--disable-autostart` generates and manages a launchd plist (macOS) or systemd user unit (Linux); cross-platform via `daemon-kit` crate
- **Doctor daemon checks** — `tracedecay doctor` now reports daemon running status and autostart configuration
- **`daemon-kit` crate** — new standalone cross-platform daemon/service toolkit published to crates.io, using `daemonize2` on Unix and `windows-service` on Windows

## [2.3.2] - 2026-03-27

### Added
- **5 new agent integrations** — Copilot (VS Code), Cursor, Zed, Cline, and Roo Code now supported via `tracedecay install --agent <id>`; each registers the MCP server in the agent's native config format (VS Code `settings.json`, `~/.cursor/mcp.json`, Zed `settings.json`, Cline/Roo Code `cline_mcp_settings.json`)
- **Auto-detect agents** — running `tracedecay install` without `--agent` detects which agents are installed by checking their config directories; if one is found it installs directly, if multiple are found an interactive checkbox selector is shown
- **Installed-agent tracking** — `installed_agents` list in `~/.tracedecay/config.toml` tracks which integrations are active; on upgrade from older versions the list is backfilled by scanning existing configs
- **Uninstall-all** — `tracedecay uninstall` without `--agent` silently removes all tracked integrations
- **JSONC parser** — VS Code and Zed settings files (JSON with comments and trailing commas) are now parsed correctly

### Changed
- **Renamed `Agent` trait to `AgentIntegration`** and all struct names from `XxxAgent` to `XxxIntegration` for consistency; functions renamed accordingly (`get_integration`, `all_integrations`, etc.)

## [2.3.1] - 2026-03-27

### Changed
- **Version-update warning suppressed for 15 minutes** — the "Update available" notice shown after `sync` and in MCP tool responses is now suppressed for 15 minutes after it was last displayed, reducing noise for frequent users; `tracedecay status` always shows the warning regardless of suppression

## [2.3.0] - 2026-03-27

### Added
- **`--skip-folder` flag for sync** — accepts one or more folder names to exclude during indexing (e.g. `tracedecay sync --skip-folder tests benches`); each folder is converted to a `folder/**` glob pattern at runtime
- **ETA during full index** — the progress spinner now shows `[current/total]` file counts and an estimated time remaining (e.g. `[12/150] indexing src/main.rs (ETA: 8s)`)

### Changed
- `index_all_with_progress` callback signature now provides `(current, total, path)` for richer progress reporting
- Schema migration re-index also shows `[current/total]` progress

## [2.2.0] - 2026-03-27

### Changed
- **Status table title split into two rows** — top row shows version (left) and country flags (right); bottom row shows token counts right-aligned in green
- **Country flags always shown** — removed `--show-flags` option; flags are now fetched automatically and cached for 30 minutes
- **Fixed table width** — cell width capped at 32 columns (max table width 100), with a derived maximum of 25 display flags
- **Upgraded gix to v0.81.0** — from v0.72.1; added explicit `sha1` feature flag and adapted to new `ControlFlow`-based tree diff API

## [2.1.0] - 2026-03-26

### Added
- **QuickBASIC 4.5 language support** — new `QuickBasicExtractor` handles `.bi` (include) and `.bm` (module) files, sharing the QBasic grammar under the existing `lang-qbasic` feature flag (31 languages total)
- **`gix` for native git operations** — replaced `Command::new("git")` shell-outs with the `gix` crate (minimal features: `revision` + `blob-diff`), removing the runtime dependency on a `git` binary for commit counting and tree diffing
- **Test coverage improvements** — 77 new tests across 6 files:
  - `complexity_test.rs` (18 tests) — direct tests for the complexity counting algorithm: branches, loops, nesting, unsafe blocks, unwrap/expect detection, assertion counting
  - `rust_extraction_test.rs` (17 tests) — Rust extractor: functions, structs, enums, traits, impls, modules, async, visibility, derive macros, call sites
  - `display_test.rs` (10 tests) — formatting functions with boundary values
  - `php_extraction_test.rs` (11 tests) — classes, interfaces, traits, namespaces, enums, visibility, inheritance
  - `ruby_extraction_test.rs` (9 tests) — classes, modules, methods, inheritance, constants, nested classes
  - `quickbasic_extraction_test.rs` (12 tests) — QB4.5-specific parsing (REDIM, SLEEP, ERASE), SUBs, FUNCTIONs, TYPEs, call sites

### Changed
- **Legacy BASIC grammars updated to 0.2.0** — `tree-sitter-qbasic`, `tree-sitter-msbasic2`, and `tree-sitter-gwbasic` bumped from 0.1 to 0.2, adding 27 new AST node types for QuickBasic 4.5 constructs (REDIM, SLEEP, ERASE, SHELL, metacommands, and more)
- `git_commits_since` now uses `gix` revision walk with `ByCommitTimeCutoff` sorting, which is more efficient than the previous `git log` approach as gix stops walking once all queued commits are older than the cutoff
- `handle_changelog` tree diff now uses `gix` tree-to-tree comparison with rename tracking, replacing `git diff --name-only`

## [2.0.3] - 2026-03-26

### Fixed
- **Windows: sync re-adding files** — normalize all relative file paths to forward slashes in the scanner, preventing path mismatch between index and sync on Windows
- **Windows: wrong upgrade command** — detect Scoop installations (`\scoop\` in binary path) and suggest `scoop update tracedecay` instead of `cargo install tracedecay`
- **Windows: git hook backslashes** — write forward slashes in `core.hooksPath` and the post-commit hook snippet, since Git's shell expects `/` separators
- **Scoop bucket structure** — moved manifest to `bucket/` subdirectory for better compatibility with `scoop update`
- **Double-counted token savings** — "Global" total no longer includes the current project's count; display now shows "Project" and "All projects" labels

## [2.0.2] - 2026-03-26

### Fixed
- COBOL tree-sitter scanner uses fixed-size arrays instead of C99 variable-length arrays, fixing MSVC compilation failure on Windows that blocked the v2.0.0 Scoop manifest update

## [2.0.0] - 2026-03-26

### Added

#### 16 new language extractors (15 → 30 languages)
- **Swift** — classes, structs, protocols, enums, extensions, init constructors, async methods, visibility modifiers, inheritance
- **Bash** — functions, `readonly` constants, `source` imports, command call sites, comment docstrings
- **Lua** — functions, colon-methods (OOP via metatables), `require()` imports, LDoc comments, `local` constants
- **Zig** — structs, enums, unions, pub/private visibility, `@import` resolution, `test` blocks as functions, doc comments
- **Protobuf** — `message` → `ProtoMessage`, `service` → `ProtoService`, `rpc` → `ProtoRpc` (new node kinds), enums, fields with type signatures, nested messages, `oneof`, package, imports
- **Nix** — functions, modules (attrsets), constants, `inherit` as imports, `apply_expression` call sites, `#` comments
- **VB.NET** — classes, structures, interfaces, modules, enums, `Sub`/`Function`, `Sub New` constructors, properties, `Inherits`/`Implements`, XML doc comments
- **PowerShell** — functions, typed constants, `Import-Module` / dot-source imports, command call sites, `<# ... #>` block comments
- **Batch/CMD** — labels as functions, `SET` as constants, `CALL :label` as call sites, `REM` docstrings (no complexity counting — too flat)
- **Perl** — `sub` functions/methods, `package` as modules, `use`/`require` imports, `our` constants, method invocations (`->`), `#` comments
- **Objective-C** — `@interface`/`@implementation`/`@protocol`, instance (`-`) and class (`+`) methods, `@property`, `NS_ENUM`, `#import`, message expression call sites, inheritance and protocol conformance
- **Fortran** — `module`, `program`, `subroutine`, `function`, derived `type` with fields, `type extends()` inheritance, `interface`, `parameter` constants, `use` imports, `!` comments
- **COBOL** — `PROGRAM-ID` as module, paragraph labels as functions, `WORKING-STORAGE` data items as fields/constants, `PERFORM` as call sites, `REM` comments (vendored grammar)
- **MS BASIC 2.0** — subroutine synthesis from `REM...RETURN` blocks, `LET` constants, `GOSUB`/`GOTO` call sites
- **GW-BASIC** — `DEF FN` functions, `WHILE/WEND` loops, subroutine synthesis, typed constants
- **QBasic** — `SUB`/`FUNCTION` blocks, `TYPE...END TYPE` as structs with fields, `CONST`, `DIM SHARED`, `CALL` sites, `SELECT CASE`

#### Enhanced Nix extraction
- **Derivation field extraction** — `mkDerivation`, `mkShell`, `buildPythonPackage`, `buildGoModule`, `buildRustPackage`, `buildNpmPackage` calls have their attrset arguments extracted as `Field` nodes (`pname`, `version`, `buildInputs`, `nativeBuildInputs`, `src`, `meta`, etc.)
- **Import path resolution** — `import ./path.nix` creates a `Use` node with a `Uses` unresolved ref, enabling cross-file dependency tracking via `tracedecay_callers` and `tracedecay_impact`
- **Flake output schema awareness** — in `flake.nix` files, standard output attributes (`packages`, `devShells`, `apps`, `nixosModules`, `nixosConfigurations`, `overlays`, `lib`, `checks`, `formatter`) are force-classified as `Module` nodes with recursive child extraction

#### Feature flag tiers
- Three compilation tiers via Cargo feature flags to control binary size:
  - **`lite`** (11 languages, always compiled): Rust, Go, Java, Scala, TypeScript/JS, Python, C, C++, Kotlin, C#, Swift
  - **`medium`** (20 languages): lite + Dart, Pascal, PHP, Ruby, Bash, Protobuf, PowerShell, Nix, VB.NET
  - **`full`** (30 languages, default): medium + Lua, Zig, Objective-C, Perl, Batch/CMD, Fortran, COBOL, MS BASIC 2.0, GW-BASIC, QBasic
- Individual `lang-*` feature flags for cherry-picking languages (e.g., `--no-default-features --features lang-nix,lang-bash`)
- `default = ["full"]` — existing users get all 30 languages with no config changes

#### New node kinds
- `ProtoMessage` — Protobuf message definitions
- `ProtoService` — Protobuf service definitions
- `ProtoRpc` — Protobuf RPC method definitions

#### Porting assessment tools
- **`tracedecay_port_status`** — compare symbols between source and target directories within the same project to track porting progress; matches by name with cross-language kind compatibility (`class` ↔ `struct`, `interface` ↔ `trait`); reports matched/unmatched/target-only counts and coverage percentage
- **`tracedecay_port_order`** — topological sort of source symbols for porting; uses Kahn's algorithm on the internal dependency graph to produce levels (port leaves first, then dependents); detects and reports dependency cycles

#### Agent prompt improvements
- **SQLite fallback instruction** — agents are told to query `.tracedecay/tracedecay.db` directly via SQL when MCP tools can't answer a code analysis question
- **Improvement feedback loop** — agents propose opening a GitHub issue when they discover an extractor/schema/tool gap, reminding the user to strip sensitive data

### Changed
- Cargo.toml `description` now lists lite-tier languages with "and many more" instead of all 30
- Vendored tree-sitter grammars for Protobuf and COBOL (no compatible crates for tree-sitter 0.26)

### Breaking
- Tree-sitter grammar dependencies for medium/full tier languages are now **optional** behind feature flags. Downstream crates depending on specific extractors must enable the corresponding `lang-*` feature.
- `cargo install tracedecay --no-default-features` now builds a **lite** binary (11 languages) instead of the previous 15. To get the old behavior, use `cargo install tracedecay` (default = full, 30 languages).
- Three new `NodeKind` variants (`ProtoMessage`, `ProtoService`, `ProtoRpc`) added — code matching exhaustively on `NodeKind` will need updating.

### Upgrade guide
```bash
cargo install tracedecay          # or: brew upgrade tracedecay
tracedecay install                # re-run to get updated prompt rules
tracedecay sync --force           # re-index to pick up new language extractors
```

## [1.10.0] - 2026-03-26

### Added
- **Version update notifications** — the MCP server checks GitHub releases (with a 5-minute cache) and warns users when a newer version is available, via both a `notifications/message` logging notification and a text block prepended to tool responses
- **Global git post-commit hook** — `tracedecay install` now offers to install a global `post-commit` hook that auto-runs `tracedecay sync` after each commit, keeping the index up to date without manual intervention
- MCP `logging` capability advertised in `initialize` response
- Minimal gitconfig parser for reading `core.hooksPath` from `~/.gitconfig` and `~/.config/git/config` without shelling out to `git`
- 12 unit tests for gitconfig parsing, insertion, and tilde expansion

## [1.8.3] - 2026-03-26

### Fixed
- OpenCode MCP config uses `mcp` key (not `mcpServers`) with `"type": "local"` and `"command": [bin, "serve"]` array format, matching the current OpenCode schema
- Removed legacy `~/.opencode.json` fallback — config always writes to `~/.config/opencode/opencode.json` (or `$XDG_CONFIG_HOME`)
- Healthcheck validates the `command` array contains `"serve"` instead of checking `args`

## [1.8.2] - 2026-03-26

### Fixed
- OpenCode config path resolution now checks `~/.config/opencode/opencode.json` (modern location) before `$XDG_CONFIG_HOME` and `~/.opencode.json` (legacy)
- OpenCode prompt path prefers `~/.config/opencode/OPENCODE.md` when the modern config directory exists

## [1.8.1] - 2026-03-26

### Added
- **OpenCode agent** (`tracedecay install --agent opencode`) — registers MCP server in `.opencode.json`, appends prompt rules to `OPENCODE.md`; healthcheck validates config and prompt file
- **Codex CLI agent** (`tracedecay install --agent codex`) — registers MCP server in `~/.codex/config.toml` with auto-approval for all 27 tools, appends prompt rules to `~/.codex/AGENTS.md`; healthcheck validates config, tool approval counts, and prompt file
- TOML helpers (`load_toml_file`, `write_toml_file`) in agents module for Codex config support
- `TOOL_NAMES` constant with bare tool names (without agent-specific prefix) for cross-agent use

### New files
- `src/agents/opencode.rs` — `OpenCodeAgent` implementing `Agent`
- `src/agents/codex.rs` — `CodexAgent` implementing `Agent`

## [1.8.0] - 2026-03-26

### Added
- **Multi-agent architecture** with a trait-based `Agent` abstraction (`install`, `uninstall`, `healthcheck`) to support CLI agents beyond Claude Code
- `tracedecay install [--agent NAME]` replaces `claude-install` — defaults to `claude` when no agent is specified
- `tracedecay uninstall [--agent NAME]` replaces `claude-uninstall` — defaults to `claude`
- `tracedecay doctor [--agent NAME]` now checks all registered agents by default; use `--agent` to narrow to one
- Agent registry with `get_agent()`, `all_agents()`, and `available_agents()` for programmatic access
- `tracedecay install --agent unknown` returns a clear error listing available agents

### Changed
- Extracted ~600 lines of Claude-specific install/uninstall/doctor logic from `main.rs` into `src/agents/claude.rs`
- Shared helpers (`load_json_file`, `write_json_file`, `which_tracedecay`, `home_dir`, `DoctorCounters`, `EXPECTED_TOOL_PERMS`) moved to `src/agents/mod.rs`
- Error messages updated from `tracedecay claude-install` to `tracedecay install`
- Backward compatibility preserved: `tracedecay claude-install` and `tracedecay claude-uninstall` still work as aliases

### New files
- `src/agents/mod.rs` — `Agent` trait, `InstallContext`, `HealthcheckContext`, `DoctorCounters`, agent registry, shared helpers
- `src/agents/claude.rs` — `ClaudeAgent` implementing `Agent`

## [1.7.1] - 2026-03-25

### Fixed
- Database schema migrations now trigger an automatic full re-index instead of printing a warning asking users to run `tracedecay sync --full` manually

### Changed
- Decomposed 6 oversized functions into small orchestrators + helpers for NASA Power of 10 Rule 4 compliance (no function exceeds 47 lines):
  - `run_doctor` (389 → 31 lines + 14 helpers)
  - `claude_install` (265 → 35 lines + 8 helpers)
  - `claude_uninstall` (160 → 16 lines + 6 helpers)
  - `print_status_table` (179 → 22 lines + 6 helpers)
  - `extract_symbols_from_query` (147 → 13 lines + helper)
  - `get_tool_definitions` (445 → 30 lines + 27 per-tool `def_*()` helpers)
- Added 84 `debug_assert!` preconditions and postconditions across 10 source files for NASA Power of 10 Rule 5 compliance (zero overhead in release builds)

## [1.7.0] - 2026-03-25

### Added
- **3 new safety metrics on every function/method node** extracted from the AST during indexing, enabling NASA Power of 10 compliance audits without grep:
  - `unsafe_blocks` — counts unsafe blocks/statements (Rust `unsafe {}`, C# `unsafe {}`)
  - `unchecked_calls` — counts force-unwrap and unchecked operations (Rust `.unwrap()`/`.expect()`, TypeScript `!`, Kotlin `!!`, Java `.get()` on Optional, Scala `.get()`, Ruby `.fetch()`)
  - `assertions` — counts assertion calls per function (Rust `assert!`/`debug_assert!`, Java `assertEquals`, Python `assertEqual`, Go `require`, C++ `EXPECT_EQ`/`ASSERT_TRUE`, and framework-specific variants for all 15 languages)
- Extended `ComplexityConfig` with 6 new fields (`unsafe_types`, `unchecked_types`, `unchecked_methods`, `call_expression_types`, `call_method_field`, `assertion_names`, `macro_invocation_types`) to support cross-language detection
- `count_complexity` now accepts source bytes for method-name and macro-name matching in call expressions
- DB migration V4 adds `unsafe_blocks`, `unchecked_calls`, and `assertions` columns to the nodes table
- `tracedecay_node` and `tracedecay_complexity` MCP tools now include the 3 new fields in their responses
- Migration log message advises users to run `tracedecay sync --full` to populate new columns for existing data

## [1.6.2] - 2026-03-25

### Fixed
- Suppressed the "new tracedecay tool(s) not yet permitted" warning when running `tracedecay claude-install`, since that command is about to fix the permissions anyway

## [1.6.1] - 2026-03-25

### Fixed
- `claude-install` now registers all 27 tool permissions — 9 tools added in v1.6.0 (`complexity`, `coupling`, `distribution`, `doc_coverage`, `god_class`, `inheritance_depth`, `largest`, `rank`, `recursion`) were missing from `EXPECTED_TOOL_PERMS`, so `claude-install` didn't grant them and `doctor` didn't flag them
- README permissions example updated to show all 27 tools (was showing only 9)
- README: fixed MCP server location reference (`~/.claude.json`, not `~/.claude/settings.json`)

## [1.6.0] - 2026-03-25

### Added
- 9 new MCP tools (27 total) for codebase analytics, code quality, and guideline compliance:
  - `tracedecay_rank` — rank nodes by relationship count with direction support (incoming/outgoing); answers "most implemented interface", "class that implements the most interfaces", etc.
  - `tracedecay_largest` — rank nodes by line count; find largest classes, longest methods
  - `tracedecay_coupling` — rank files by fan-in (most depended-on) or fan-out (most dependencies)
  - `tracedecay_inheritance_depth` — find deepest class hierarchies via recursive CTE on extends chains
  - `tracedecay_distribution` — node kind breakdown per file/directory with summary mode
  - `tracedecay_recursion` — detect recursive/mutually-recursive call cycles (NASA Power of 10, Rule 1)
  - `tracedecay_complexity` — rank functions by composite complexity score with real cyclomatic complexity from AST
  - `tracedecay_doc_coverage` — find public symbols missing documentation (Rust guidelines M-CANONICAL-DOCS)
  - `tracedecay_god_class` — find classes with the most members (methods + fields)
- **Complexity metrics on every function/method node** — 4 new columns extracted from the AST during indexing:
  - `branches` — branching statements (if, match/switch arms, ternary, catch). CC = branches + 1.
  - `loops` — loop constructs (for, while, loop, do). Enables NASA Rule 2 audits.
  - `returns` — early exits (return, break, continue, throw).
  - `max_nesting` — deepest brace nesting level. Enables NASA Rule 1 (≤4 levels) audits.
- Generic `count_complexity()` helper with per-language configs for all 15 supported languages
- DB migration V3 adds the 4 complexity columns to the nodes table
- All new tools use efficient SQL queries (JOINs, GROUP BY, recursive CTEs) instead of loading all edges into memory

## [1.5.4] - 2026-03-25

### Fixed
- Token counter inflation: `tracedecay_files` no longer accumulates tokens saved (listing file names is metadata, not a file-read substitute)
- Worldwide counter staleness: periodic flush every 30 seconds during MCP sessions instead of only on shutdown
- Shutdown flush was effectively a no-op (delta always 0 because `accumulate_tokens_saved` already upserted the current value to global DB); now uses `last_flushed_tokens` to correctly track remaining delta

## [1.5.1] - 2026-03-25

### Added
- `tracedecay doctor` command — comprehensive health check of binary, project index, global DB, user config, Claude Code integration (MCP server, hook, permissions, CLAUDE.md), and network connectivity
- Stale install warning: automatically detects when `claude-install` needs re-running due to new tool permissions and warns on every CLI command

### Added
- 9 new MCP tools (18 total):
  - `tracedecay_dead_code` — find unreachable symbols with no incoming edges
  - `tracedecay_diff_context` — semantic context for changed files (modified symbols, dependencies, affected tests)
  - `tracedecay_module_api` — public API surface of a file or directory
  - `tracedecay_circular` — detect circular file dependencies
  - `tracedecay_hotspots` — most connected symbols by edge count
  - `tracedecay_similar` — find symbols with similar names
  - `tracedecay_rename_preview` — all references to a symbol
  - `tracedecay_unused_imports` — import statements never referenced
  - `tracedecay_changelog` — semantic diff between two git refs
- `get_all_edges()`, `get_nodes_by_file()`, `get_all_nodes()`, `get_incoming_edges()`, `get_outgoing_edges()` delegation methods on `TraceDecay`
- `find_circular_dependencies()` graph query for file-level cycle detection
- `tracedecay status` prompts to create index if none exists (Y/n)
- Country flags in status output via `--show-flags`

## [1.4.3] - 2026-03-25

### Added
- Country flags row in `tracedecay status` — shows emoji flags of countries where tracedecay is used, centered below the token counters
- `fetch_country_flags()` in cloud module (500ms timeout, best-effort)
- Flags truncated with ellipsis if they exceed the available table width

## [1.4.2] - 2026-03-25

### Added
- PHP language support (`.php`) — functions, classes, methods, traits, interfaces, enums, constants, properties, namespaces, imports, and call sites
- Ruby language support (`.rb`) — methods, classes, modules, constants, inheritance, and call sites

## [1.4.1] - 2026-03-25

### Added
- Cross-platform release workflow — GitHub Actions builds prebuilt binaries for macOS (ARM), Linux (x86_64, ARM64), and Windows (x86_64) on every release
- Scoop package manager support for Windows (`scoop install tracedecay`)
- Automated Scoop bucket updates on release
- Automated Homebrew formula + bottle updates on release

### Changed
- README updated with all install methods (brew, scoop, cargo, prebuilt binaries)

## [1.4.0] - 2026-03-25

### Added
- Worldwide tracedecayd counter — aggregates anonymous token counts across all tracedecay users via Cloudflare Worker + Upstash Redis
- `tracedecay status` shows three tiers: Local, Global, and Worldwide token counts
- `tracedecay disable-upload-counter` / `tracedecay enable-upload-counter` commands to opt out of uploading
- All upload state stored transparently in `~/.tracedecay/config.toml`
- Version check on `status` (5-min cache) and `sync` (parallel, no added latency) with auto-detected upgrade command (cargo/brew)
- First-run notice informing users about the worldwide counter and how to opt out
- Flush cooldown (60s) after failed uploads to prevent sluggish CLI during outages
- Network Calls & Privacy section in README documenting all outbound requests

### Changed
- `update_global_db()` now computes tracedecayd deltas for accurate pending upload accumulation
- Moved Cloudflare Worker source to separate `tracedecay-cloud` repository

## [1.3.0] - 2026-03-24

### Added
- User-level global database (`~/.tracedecay/global.db`) that tracks all TraceDecay projects and their cumulative saved tokens
- `tracedecay_status` and CLI `tracedecay status` now report both local (project) and global (all projects) tokens saved when the global DB is available
- All CLI entry points (`sync`, `status`, `claude-install` init) register the project in the global DB on every run
- MCP server updates the global DB on every token accumulation and on shutdown (best-effort, no locking)

### Changed
- `print_status_table` title row shows `Local ~X  Global ~Y` when global data is available, falls back to `Tokens saved ~X` otherwise

## [1.2.1] - 2026-03-24

### Fixed
- Renamed all remaining `codegraph` references in release workflow, Homebrew formula, setup script, and hook to `tracedecay`
- Release workflow now produces `tracedecay` binary, bottles, and source tarballs (was still using `codegraph` names)
- Homebrew formula class renamed from `Codegraph` to `TraceDecay` with updated URLs
- Setup script variable `CODEGRAPH_BIN` renamed to `TRACEDECAY_BIN`
- CLAUDE.md marker in setup script updated to use `TraceDecay` name

## [1.2.0] - 2026-03-24

### Added
- `claude-install` CLI command — configures Claude Code integration (MCP server, permissions, hook, CLAUDE.md rules) in a single step, replacing the bash `setup.sh` script
- `hook-pre-tool-use` hidden CLI command — cross-platform PreToolUse hook handler written in pure Rust (no bash/jq dependency), blocks Explore agents and exploration-style prompts

### Removed
- Embedded bash hook script — the hook is now a native Rust subcommand

## [1.1.0] - 2026-03-24

### Added
- `tracedecay files` CLI command — list indexed files with `--filter` (directory prefix), `--pattern` (glob), and `--json` output
- `tracedecay affected` CLI command — BFS through file dependency graph to find test files impacted by source changes; supports `--stdin` (pipe from `git diff --name-only`), `--depth`, `--filter`, `--json`, `--quiet`
- `tracedecay_files` MCP tool — file listing with path/pattern filtering, flat or grouped-by-directory output
- `tracedecay_affected` MCP tool — find affected test files via file-level dependency traversal
- Graceful shutdown handler for MCP server — persists tokens-saved counter, checkpoints SQLite WAL, and logs session summary on SIGINT/SIGTERM
- `Database::checkpoint()` method for WAL cleanup on shutdown

## [1.0.1] - 2026-03-24

### Changed
- Increased ANSI logo size by 25%

## [1.0.0] - 2026-03-24

### Changed
- **Renamed project from `token-codegraph` to `tracedecay`**
- Crate name: `tracedecay` (was `token-codegraph`)
- Binary name: `tracedecay` (was `codegraph`)
- Data directory: `.tracedecay/` (was `.codegraph/`)
- MCP tool prefix: `tracedecay_*` (was `codegraph_*`)
- Version bump to 1.0.0

### Added
- TypeScript/JavaScript language support (.ts, .tsx, .js, .jsx)
- Python language support (.py)
- C language support (.c, .h)
- C++ language support (.cpp, .hpp, .cc, .cxx, .hh)
- Kotlin language support (.kt, .kts)
- Dart language support (.dart)
- C# language support (.cs)
- Pascal language support (.pas, .pp, .dpr)
- Legacy `.codegraph/` directory detection with migration warning
- CHANGELOG.md for tracking version history

## [0.6.0]

### Added
- Scala language support (.scala, .sc)

### Fixed
- Self-animating spinner with cursor hiding and path truncation
- Show each language as its own cell in status table

### Changed
- Show indexed languages in status, fix multi-language file discovery

## [0.5.2]

### Changed
- Update repo URLs after GitHub rename to tracedecay
- Rename crate to tracedecay for crates.io

## [0.5.1]

### Added
- Compact bordered table for status output

## [0.5.0]

### Added
- Java language support (.java)
- Go language support (.go)
- ANSI logo and crates.io readiness

### Changed
- NASA rules compliance improvements

## [0.4.2]

### Added
- Versioned DB migration system with exclusive locking

### Fixed
- Create metadata table on open for existing databases

## [0.4.1]

### Added
- Show version number in tracedecay status
- Persist tokens-saved counter to database
- Show indexed token count in tracedecay status

### Changed
- Update dependencies

## [0.4.0]

### Added
- Initial Rust language support (.rs)
- Replace rusqlite with native libsql (Turso) crate
- Sync progress spinner and post-commit hook
- Prompt to create index when invoked with no command
- Install section with setup script and hooks

### Changed
- Replace `index` command with `sync --force`

## [0.3.0]

### Added
- MCP tool call logging to stderr
- Merge init and index into a single command

### Fixed
- Harden MCP inputs and prevent path traversal

## [0.2.0]

### Added
- Go extractor with deep extraction support
- Java extractor with deep extraction support
- LanguageExtractor trait and LanguageRegistry for multi-language dispatch
- Runtime stats tracking to MCP server
- Homebrew release workflow

### Fixed
- Sanitize FTS5 search queries to handle special characters
- Address code review findings (UTF-8 safety, FK violations, stats accuracy)

## [0.1.0]

### Added
- MCP server (JSON-RPC 2.0 over stdio)
- CLI interface and TraceDecay orchestrator
- Vector embeddings for semantic search
- Context builder for AI-ready code graph context
- Incremental sync for detecting file changes
- Graph traversal and query operations
- Reference resolution module
- Tree-sitter Rust extraction module
- libsql database layer with full CRUD operations
- Configuration module with glob-based file filtering
- Core types and error handling scaffold
[6.1.1]: https://github.com/ScriptedAlchemy/tracedecay/releases/tag/v6.1.1
[--help]: https://github.com/ScriptedAlchemy/tracedecay/releases/tag/v--help
[0.0.2]: https://github.com/ScriptedAlchemy/tracedecay/releases/tag/v0.0.2
