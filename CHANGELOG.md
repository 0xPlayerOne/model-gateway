# Changelog

## [0.14.4](https://github.com/0xPlayerOne/model-gateway/compare/v0.14.3...v0.14.4) (2026-08-03)


### Maintenance

* **ci:** align Code Foundry release policy ([#142](https://github.com/0xPlayerOne/model-gateway/issues/142)) ([b74ae0e](https://github.com/0xPlayerOne/model-gateway/commit/b74ae0e8fe1ded4f01e49cb14aa5cb25e02c90b0))

## [0.14.3](https://github.com/0xPlayerOne/model-gateway/compare/v0.14.2...v0.14.3) (2026-08-03)


### Maintenance

* **staging:** align branch with main ([2670ace](https://github.com/0xPlayerOne/model-gateway/commit/2670aced0e21186c9b864c7339dbd1839f2d8ed2))

## [0.14.2](https://github.com/0xPlayerOne/model-gateway/compare/v0.14.1...v0.14.2) (2026-08-02)


### Bug Fixes

* **ci:** align release toolchain with sqlite dependencies ([a5a6c3e](https://github.com/0xPlayerOne/model-gateway/commit/a5a6c3efeb7d6ad920dd0bb1e147feb3181de751))
* **release:** version the OpenAPI contract ([ebed12c](https://github.com/0xPlayerOne/model-gateway/commit/ebed12cdcecab8b9682561acd6542dd806f92429))

## [0.14.1](https://github.com/0xPlayerOne/model-gateway/compare/v0.14.0...v0.14.1) (2026-08-02)


### Bug Fixes

* **ci:** skip optional scans for release metadata ([9ee29a2](https://github.com/0xPlayerOne/model-gateway/commit/9ee29a2fc1af541d95146f3f91c05e86a08a78a7))
* **release:** configure root manifest package ([5f6f9ce](https://github.com/0xPlayerOne/model-gateway/commit/5f6f9ce624f0cd63739d4ef29d50999b43fea14c))


### Performance

* **gateway:** offload routing reads and invalidate catalog cache ([4e8eba1](https://github.com/0xPlayerOne/model-gateway/commit/4e8eba18b91b6d5d38e1bf4d07690843a811ce7f))


### CI

* **release:** publish versioned artifacts ([0b21109](https://github.com/0xPlayerOne/model-gateway/commit/0b21109f9badc3542aad33c62210f3e412928947))

## [0.14.0](https://github.com/0xPlayerOne/model-gateway/compare/v0.13.0...v0.14.0) (2026-08-02)

### Features

* **api:** add deterministic field projection, cursor pagination, model links, and conditional catalog retrieval ([6f99e02](https://github.com/0xPlayerOne/model-gateway/commit/6f99e0277e75e45957ad9ae26ad2a0175cde1a23))
* **runtime:** add explicit CLIProxy port configuration, OAuth readiness diagnostics, and supervised launchers ([6f99e02](https://github.com/0xPlayerOne/model-gateway/commit/6f99e0277e75e45957ad9ae26ad2a0175cde1a23))

### Performance Improvements

* **catalog:** derive list validators from request dimensions and return conditional responses before materializing catalog data ([6f99e02](https://github.com/0xPlayerOne/model-gateway/commit/6f99e0277e75e45957ad9ae26ad2a0175cde1a23))

### Bug Fixes

* **api:** preserve stable `Last-Modified` values when benchmark timestamps are unavailable ([6f99e02](https://github.com/0xPlayerOne/model-gateway/commit/6f99e0277e75e45957ad9ae26ad2a0175cde1a23))
* **build:** align the container builder with the repository Rust toolchain and include the OpenAPI contract in the image ([6f99e02](https://github.com/0xPlayerOne/model-gateway/commit/6f99e0277e75e45957ad9ae26ad2a0175cde1a23))

### Documentation

* **docs:** clarify catalog response shapes, benchmark provenance, provider readiness, and startup behavior ([6f99e02](https://github.com/0xPlayerOne/model-gateway/commit/6f99e0277e75e45957ad9ae26ad2a0175cde1a23))

## [0.13.0](https://github.com/0xPlayerOne/model-gateway/compare/v0.12.0...v0.13.0) (2026-08-02)


### Features

* **api:** provide compact model resources with links, deterministic field selection, cursor pagination, and conditional caching ([#103](https://github.com/0xPlayerOne/model-gateway/pull/103))
* **catalog:** expose reasoning-effort-specific quality, benchmark, and pricing metadata ([#103](https://github.com/0xPlayerOne/model-gateway/pull/103))
* **cli:** manage CLIProxyAPI setup, OAuth supervision, explicit secret stores, and readiness checks ([#103](https://github.com/0xPlayerOne/model-gateway/pull/103))

### Performance Improvements

* **catalog:** reuse normalized identity indexes and short-lived snapshots during reconciliation and catalog assembly ([#103](https://github.com/0xPlayerOne/model-gateway/pull/103))
* **identity:** fetch identity sources concurrently ([#97](https://github.com/0xPlayerOne/model-gateway/issues/97)) ([5839314](https://github.com/0xPlayerOne/model-gateway/commit/583931457fe62176c038e9650824d326b59adf65))

### Bug Fixes

* **routing:** use measured benchmark signals and latency-aware Pareto selection for automatic model routing ([#103](https://github.com/0xPlayerOne/model-gateway/pull/103))
* **security:** avoid logging secret-derived CLIProxy metadata ([#105](https://github.com/0xPlayerOne/model-gateway/pull/105))

### Documentation

* **docs:** clarify installation, startup responsibilities, OAuth setup, catalog fields, pagination, and caching ([#103](https://github.com/0xPlayerOne/model-gateway/pull/103))

### CI

* **ci:** replace stale validation callers with a tiered Code Foundry workflow and a single required gate ([#103](https://github.com/0xPlayerOne/model-gateway/pull/103))

## [0.12.0](https://github.com/0xPlayerOne/model-gateway/compare/v0.11.0...v0.12.0) (2026-07-29)


### Features

* **api:** add compact linked model resources ([#56](https://github.com/0xPlayerOne/model-gateway/issues/56)) ([055b2b6](https://github.com/0xPlayerOne/model-gateway/commit/055b2b6957f21fc67a629c4f99e4dec68b7efb77))
* **api:** add compact linked model resources ([#56](https://github.com/0xPlayerOne/model-gateway/issues/56)) ([1f6387a](https://github.com/0xPlayerOne/model-gateway/commit/1f6387a521a43193fb0196e2186ea01e5438ed9f))
* **api:** expose benchmark metrics in model resources ([0a8beeb](https://github.com/0xPlayerOne/model-gateway/commit/0a8beebbb7aa0dbba3f5c851607456c5e9432393))
* **api:** expose benchmark metrics in model resources ([b1291ca](https://github.com/0xPlayerOne/model-gateway/commit/b1291caaaf2440c79c8f92bea82748dfe42bfa31))
* **api:** retire legacy catalog routes ([8073e3f](https://github.com/0xPlayerOne/model-gateway/commit/8073e3fba845e885f1c2e0604160b7e347f15ed1))
* **api:** retire legacy catalog routes ([#61](https://github.com/0xPlayerOne/model-gateway/issues/61)) ([90142ca](https://github.com/0xPlayerOne/model-gateway/commit/90142ca6aa7894a9fa15985d0e479137231e75f0))
* **api:** unify catalog discovery resources ([#58](https://github.com/0xPlayerOne/model-gateway/issues/58)) ([dce5f6e](https://github.com/0xPlayerOne/model-gateway/commit/dce5f6e3f1d195d2e13ac3060a4d213f059b338a))
* **catalog:** expand reasoning effort variants ([cb1d99c](https://github.com/0xPlayerOne/model-gateway/commit/cb1d99cd3b0b5ccc41446abef789161c95ac230d))
* **catalog:** expand reasoning effort variants ([1af8c6d](https://github.com/0xPlayerOne/model-gateway/commit/1af8c6d7032a256d84875d87cc8c4df295eea49c))
* **cli:** manage CLIProxyAPI as a background service ([#75](https://github.com/0xPlayerOne/model-gateway/issues/75)) ([fab9ea0](https://github.com/0xPlayerOne/model-gateway/commit/fab9ea04d94807f59899011a48eb13e0ff10b065))
* reduce code-foundry consumer footprint ([0b5b08b](https://github.com/0xPlayerOne/model-gateway/commit/0b5b08be48778462527ff1946a84370b22729b91))
* reduce code-foundry consumer footprint ([b687df8](https://github.com/0xPlayerOne/model-gateway/commit/b687df82dff2c2c880ac5b4ad21fb7e4faf1049a))
* **routing:** prioritize latency on frontier ([ad8e021](https://github.com/0xPlayerOne/model-gateway/commit/ad8e021bcf86e597a0db7a39c0772166a16145fc))
* **routing:** prioritize latency on frontier ([f2f30f9](https://github.com/0xPlayerOne/model-gateway/commit/f2f30f9f5e6bce9773ae9171db36c120f077fb4e))
* **routing:** raise frontier quality floor ([d54a473](https://github.com/0xPlayerOne/model-gateway/commit/d54a47315ffdf127ead02ce0effaabaa64b4def0))
* **routing:** raise frontier quality floor ([9ad0938](https://github.com/0xPlayerOne/model-gateway/commit/9ad09380657df5b24ec4bc50b1483f526ac4b620))
* **routing:** use measured task cost for frontiers ([e028aa0](https://github.com/0xPlayerOne/model-gateway/commit/e028aa020dbd915b50ed63526378ec282121479e))
* **routing:** use measured task cost for frontiers ([85f35fd](https://github.com/0xPlayerOne/model-gateway/commit/85f35fdf8a37bb9c8ac08687a7dcb5d3301af6fb))


### Bug Fixes

* **api:** expose current models and contract routes ([#76](https://github.com/0xPlayerOne/model-gateway/issues/76)) ([f515fff](https://github.com/0xPlayerOne/model-gateway/commit/f515fff601c97692ba4ef88f03612dc3b537c69f))
* **api:** harden release behavior and compact summaries ([4991136](https://github.com/0xPlayerOne/model-gateway/commit/49911363f9b2e2746f5de4b22df0321cf0a17812))
* **ci:** remove unused Python validation profile ([38693c6](https://github.com/0xPlayerOne/model-gateway/commit/38693c6194fd22838854a8cecc30081f7496b74d))
* remove obsolete consumer helper ([5293809](https://github.com/0xPlayerOne/model-gateway/commit/52938093c654d408fd6c9271abcfde57894592e1))
* remove obsolete consumer helper ([4b0bd55](https://github.com/0xPlayerOne/model-gateway/commit/4b0bd55edf4a7beaa9251f3455244fb90502da49))
* **routing:** separate measured and estimated task costs ([df2a276](https://github.com/0xPlayerOne/model-gateway/commit/df2a27680a33de5556fc91170f7fd9c91031071e))
* **routing:** separate measured and estimated task costs ([4a143d3](https://github.com/0xPlayerOne/model-gateway/commit/4a143d35bfb5f5f81684a0ccb81373f37a4d8c73))

## [0.11.0](https://github.com/0xPlayerOne/model-gateway/compare/v0.10.1...v0.11.0) (2026-07-28)


### Features

* preserve cache pricing metadata ([402d818](https://github.com/0xPlayerOne/model-gateway/commit/402d8182ecbae4b18f74baf6fa14dd49abcd05a2))

## [0.10.1](https://github.com/0xPlayerOne/model-gateway/compare/v0.10.0...v0.10.1) (2026-07-28)


### Bug Fixes

* cover canonical price observation mapping ([bc89370](https://github.com/0xPlayerOne/model-gateway/commit/bc89370a793e12d1ea15fe48f8bf312a0aae9507))
* cover canonical price observation mapping ([e92fb4b](https://github.com/0xPlayerOne/model-gateway/commit/e92fb4bcf54b76999ce5b5259e50887b10efc1fb))

## [0.10.0](https://github.com/0xPlayerOne/model-gateway/compare/v0.9.0...v0.10.0) (2026-07-28)


### Features

* improve model reconciliation and pricing coverage ([8d43001](https://github.com/0xPlayerOne/model-gateway/commit/8d430017c6e10b7e7553c3ffdbe1dde9b6725908))

## [0.9.0](https://github.com/0xPlayerOne/model-gateway/compare/v0.8.0...v0.9.0) (2026-07-28)


### Features

* add OAuth subscription sidecar ([7eaa3c9](https://github.com/0xPlayerOne/model-gateway/commit/7eaa3c96e1bca3c9ad566164b74548ce157345ab))


### Bug Fixes

* avoid OAuth metadata logging ([5e8161d](https://github.com/0xPlayerOne/model-gateway/commit/5e8161d88dd6e3e1689a61034522d9ec745eb127))
* avoid OAuth metadata logging ([5131280](https://github.com/0xPlayerOne/model-gateway/commit/51312803f80aa1304a64af76feae34291946b8b7))
* **security:** provide stable Python audit gate ([770dff7](https://github.com/0xPlayerOne/model-gateway/commit/770dff77b9e5b87589758b267ce1f272077722b1))


### Performance Improvements

* **security:** use preinstalled Rust toolchain ([f8424c8](https://github.com/0xPlayerOne/model-gateway/commit/f8424c8e5168354a8bce7d90cd87a09da76b2e5b))

## [0.8.0](https://github.com/0xPlayerOne/model-gateway/compare/v0.7.0...v0.8.0) (2026-07-28)


### Features

* distinguish free-tier access from zero pricing ([dc886bb](https://github.com/0xPlayerOne/model-gateway/commit/dc886bbbd4c2eb71bbf7909310fbf995274fa5a9))


### Bug Fixes

* **security:** skip unchanged ecosystem audits ([5f61fac](https://github.com/0xPlayerOne/model-gateway/commit/5f61fac3f8a8f19d31797bf0689ff2bcfb4f1827))


### Performance Improvements

* **ci:** avoid formatter probe overhead ([a67c708](https://github.com/0xPlayerOne/model-gateway/commit/a67c708cfea80a32f69c5dd7f97c74c40bb214ba))
* **ci:** bootstrap direct prettier ([7be371a](https://github.com/0xPlayerOne/model-gateway/commit/7be371a15b3db87a9c4cf84ed302823d378f3ef0))
* **ci:** skip Bun cache archives for workspaces ([2a3052b](https://github.com/0xPlayerOne/model-gateway/commit/2a3052b44e34214b17f5b5710117ee19cec5508a))
* **ci:** skip installs on turbo cache hits ([c5eb8f4](https://github.com/0xPlayerOne/model-gateway/commit/c5eb8f466b03d1ac2f5cf212190eef43770a3127))
* **security:** guard empty Python audit matrix ([65d09e6](https://github.com/0xPlayerOne/model-gateway/commit/65d09e6f000d83c9c4169d72af75d9471885831c))
* **template:** speed up CodeQL detection ([5d2c0d2](https://github.com/0xPlayerOne/model-gateway/commit/5d2c0d215ad9f08fd6cbecbea9f9a6b373cb6144))
* **template:** use current Turbo remote cache mode ([36ab7d5](https://github.com/0xPlayerOne/model-gateway/commit/36ab7d5b883a774e583ec7c0413c8dccd99354f9))

## [0.7.0](https://github.com/0xPlayerOne/model-gateway/compare/v0.6.0...v0.7.0) (2026-07-28)


### Features

* add source-backed model identity registry ([3b93a3e](https://github.com/0xPlayerOne/model-gateway/commit/3b93a3ebdaf7733c0c9bec10d9983d02aad629ee))
* aggregate provider-scoped model pricing ([fa85e71](https://github.com/0xPlayerOne/model-gateway/commit/fa85e718f5c98fd5df79c2521e211582d7eaca8c))


### Bug Fixes

* **cache:** detect restored Bun installs ([3a9dedd](https://github.com/0xPlayerOne/model-gateway/commit/3a9deddc9615c74685859f3cfd8a1a08cd8deb13))
* **cache:** require exact Bun dependency matches ([7103e4d](https://github.com/0xPlayerOne/model-gateway/commit/7103e4db207b19efa45030ac8900e4946ae8c52a))
* **ci:** activate build cache for build task ([e71c671](https://github.com/0xPlayerOne/model-gateway/commit/e71c671f05ae36df75ed1eb7d55b329568a63e37))
* **ci:** always activate detected build cache ([a2f0ed7](https://github.com/0xPlayerOne/model-gateway/commit/a2f0ed7056cd81c916a241f732253fbd34f25981))
* **ci:** stabilize Python audit status ([538e601](https://github.com/0xPlayerOne/model-gateway/commit/538e601fb5dc8cf2b665249d80ea4b88431b8ba5))
* **codeql:** restore valid matrix workflow ([30181c5](https://github.com/0xPlayerOne/model-gateway/commit/30181c57bcc5e6f29c430a9d4485df2111b8697d))
* correct project name in license ([324eb23](https://github.com/0xPlayerOne/model-gateway/commit/324eb23e785d32da9ecd72c7a53b03ffcc06e56d))
* detect native Bun coverage runners ([d3cda4e](https://github.com/0xPlayerOne/model-gateway/commit/d3cda4ebbfe293d938bcbffbf1a05f154d4c06d9))
* enforce aggregate coverage in CI ([e635361](https://github.com/0xPlayerOne/model-gateway/commit/e635361ba1dd353c1be9fc0e2eaeae332cc184f5))
* enforce function and line coverage ([9436163](https://github.com/0xPlayerOne/model-gateway/commit/94361631e17712193109cd3b67c8f46a1cbc2b47))
* hermetic CLI tests + deduplicate civil_from_days ([#15](https://github.com/0xPlayerOne/model-gateway/issues/15)) ([41c9bc8](https://github.com/0xPlayerOne/model-gateway/commit/41c9bc83f2a511a7912bfee1257a09594a1ba167))
* ignore shared helper files in language detection ([ba5e093](https://github.com/0xPlayerOne/model-gateway/commit/ba5e0935d9e9bbd053d92c37685a44758429ee6e))
* install Rust CI components ([72f2600](https://github.com/0xPlayerOne/model-gateway/commit/72f2600a2c22f50b008458010fd41474550e326d))
* install rustfmt and clippy components in CI workflow ([89bd6d2](https://github.com/0xPlayerOne/model-gateway/commit/89bd6d2d49fbbb2639a617280ed687dd66b8fad9))
* isolate Python coverage from mise environment ([87d0cbe](https://github.com/0xPlayerOne/model-gateway/commit/87d0cbe120b2fc5233f675c8fc5ee1c124cd85b7))
* isolate Python coverage sysconfig workaround ([3bd29f8](https://github.com/0xPlayerOne/model-gateway/commit/3bd29f81a86d810f665ae482e7a8fb8453bf0970))
* make benchmark identity matching fail closed ([2d02eeb](https://github.com/0xPlayerOne/model-gateway/commit/2d02eeb40f3bc450c5eac4aa2968de37b3612b0e))
* pass explicit Python coverage sources ([033d26e](https://github.com/0xPlayerOne/model-gateway/commit/033d26e79cc53b7043ae4e1d2e56110f21075301))
* preserve free pricing and fallback depth ([1516237](https://github.com/0xPlayerOne/model-gateway/commit/151623783445fadce86075cd905791a2c09a4b75))
* restore 2-space indent on release body bullet ([24c2bc1](https://github.com/0xPlayerOne/model-gateway/commit/24c2bc1a9a473847a1b357a395d17ef019fa0147))
* restore dependabot open-pull-requests-limit ([193f292](https://github.com/0xPlayerOne/model-gateway/commit/193f292d308d37ad1234a12929758f70d9bf2994))
* restore release artifact pipeline ([784a9db](https://github.com/0xPlayerOne/model-gateway/commit/784a9dbcfedba4e302305ee1d5d691d19ebc8a58))
* **security:** audit conflicting Python manifests independently ([0f14729](https://github.com/0xPlayerOne/model-gateway/commit/0f147294a8e01b3689a1c67e2ab4db609f87ab69))
* **security:** detect all Python requirement manifests ([88fb79b](https://github.com/0xPlayerOne/model-gateway/commit/88fb79bfea86221b8b193a3a359329bd34fc6701))
* **security:** name skipped Python audits clearly ([fca2490](https://github.com/0xPlayerOne/model-gateway/commit/fca2490ae67793492bb63f37cc1f5fcdda7dbbdf))
* **security:** restore stable Python audit check ([56926ea](https://github.com/0xPlayerOne/model-gateway/commit/56926eaed3a79e5d8ff084aa1cb974a92671a752))
* **setup:** correct action yaml indentation ([33236d3](https://github.com/0xPlayerOne/model-gateway/commit/33236d3e8c9bdf39ec3af984b01af73a53b8834e))
* **setup:** pin uv action for Python workflows ([5267bcb](https://github.com/0xPlayerOne/model-gateway/commit/5267bcb25d17535af8de71ec9ad554b4c6bde473))
* stabilize coverage and dependency audits ([8c1b040](https://github.com/0xPlayerOne/model-gateway/commit/8c1b04001029c705e8a9f097cf8273a6c4e161f5))
* support macOS bash in CI helpers ([84c53f4](https://github.com/0xPlayerOne/model-gateway/commit/84c53f4385c9edaba5e49d2b7be7d103deb19c6f))
* support native and jest coverage reports ([821be87](https://github.com/0xPlayerOne/model-gateway/commit/821be870839de79d7152946b4027428685f081e2))
* use workflow token for release PRs ([2757446](https://github.com/0xPlayerOne/model-gateway/commit/2757446946da7389269e6cd5a0299bbe4621e013))


### Performance Improvements

* **cache:** allow measured Bun package overrides ([a7bb70f](https://github.com/0xPlayerOne/model-gateway/commit/a7bb70f1da5b01f65b61fb7b999298b0bcc298be))
* **cache:** avoid duplicate parallel saves ([c7cda3d](https://github.com/0xPlayerOne/model-gateway/commit/c7cda3d77c9f15d481f4d548ead10f392015d06c))
* **cache:** bound Bun dependency archives ([de44eb3](https://github.com/0xPlayerOne/model-gateway/commit/de44eb3d42492227a53c0f1d737ece301e3e07cf))
* **cache:** honor restore-only workflow jobs ([c1c74c3](https://github.com/0xPlayerOne/model-gateway/commit/c1c74c37e6526e5a2e6589292ee7ac33a087c8b4))
* **ci:** adapt package cache to lockfile size ([700d332](https://github.com/0xPlayerOne/model-gateway/commit/700d33211b878d3c95473e095264b8b7efebb2b7))
* **ci:** apply repo-foundry workflow optimizations ([7c9a5e9](https://github.com/0xPlayerOne/model-gateway/commit/7c9a5e93a8e4838f84f3b1a2aaadf0f6e948fc63))
* **ci:** avoid unprofitable js build caches ([1e56f1c](https://github.com/0xPlayerOne/model-gateway/commit/1e56f1c1f92bdae8f494d443e6005d59a05b14e4))
* **ci:** cache Rust lint artifacts ([18b7ebc](https://github.com/0xPlayerOne/model-gateway/commit/18b7ebcfb565df12fa8de1723960ffb3cb09e482))
* **ci:** default to preloaded runner ([0d8d77a](https://github.com/0xPlayerOne/model-gateway/commit/0d8d77a71e1712ee3b14311c03eda9db4c0b3ba1))
* **ci:** default Unit tests to slim runner ([a391065](https://github.com/0xPlayerOne/model-gateway/commit/a3910654f3735670d909ddec0c1ae310e196caff))
* **ci:** enable framework build caches ([bbc373a](https://github.com/0xPlayerOne/model-gateway/commit/bbc373a02e832707e91d733118f55698ee3d85c3))
* **ci:** scope turbo cache to active task ([3347e84](https://github.com/0xPlayerOne/model-gateway/commit/3347e846d472d6a0173250bbd1730fc0e3fb079a))
* **ci:** use lean runner for format and lint ([d8ec81b](https://github.com/0xPlayerOne/model-gateway/commit/d8ec81bd05c477dbd021ef99aa13031d48867de0))
* **codeql:** skip unchanged analyzers before runners ([59fcd99](https://github.com/0xPlayerOne/model-gateway/commit/59fcd9983241ad72bec1d5593da91960182d28dc))
* **experiment:** share Rust build cache ([82a9646](https://github.com/0xPlayerOne/model-gateway/commit/82a964640d2f4c8be7293860c9380be4b8f33d15))
* **python:** avoid duplicate uv setup ([9d838c1](https://github.com/0xPlayerOne/model-gateway/commit/9d838c139544ae99fce3ebd875db3cd0380bcb6d))
* **release:** build Intel macOS on arm runner ([23456ed](https://github.com/0xPlayerOne/model-gateway/commit/23456ed2b03ca61fd664cfa054fe1fac29ff358d))
* **release:** cache container builds ([2ddf1f1](https://github.com/0xPlayerOne/model-gateway/commit/2ddf1f11ac2c6aec90b9e2f3ceb1d909898ebed4))
* **release:** cancel superseded runs ([cdf0ca1](https://github.com/0xPlayerOne/model-gateway/commit/cdf0ca1ba337da4d6f6e272d4519e97274debe05))
* **release:** parallelize validation and cache cargo ([95c671e](https://github.com/0xPlayerOne/model-gateway/commit/95c671e07f761cb07a7393f8651e83a50c9fbe27))
* **release:** reuse validated native binary ([434a847](https://github.com/0xPlayerOne/model-gateway/commit/434a847f8587d448fa31088126d048583ed1f41b))
* **release:** skip redundant archive compression ([55fe56a](https://github.com/0xPlayerOne/model-gateway/commit/55fe56a387ec639477d9b91d6a5fb8a8ca6b4fcc))
* **security:** audit all Python requirements once ([fc1e284](https://github.com/0xPlayerOne/model-gateway/commit/fc1e284af6302a8ce671e24fa1571fc3cf272891))
* **security:** bootstrap uv for Python audits ([d094ea3](https://github.com/0xPlayerOne/model-gateway/commit/d094ea3a6c0f5f6d944fe62a6405d1c6555c6250))
* **security:** cache pinned Python auditor ([9c7c116](https://github.com/0xPlayerOne/model-gateway/commit/9c7c1168d22191a53e7650ab5114652ce3073267))
* **security:** cache Rust toolchains ([b3fa8ae](https://github.com/0xPlayerOne/model-gateway/commit/b3fa8ae485ffbd6755f110f4e2b360e8f68f23fc))
* **security:** fan out Python dependency audits ([40981fe](https://github.com/0xPlayerOne/model-gateway/commit/40981feb2078abf7e26e88673ad4cdb746ffad7f))
* **security:** gate audits from shared profile ([97453c3](https://github.com/0xPlayerOne/model-gateway/commit/97453c33ab15c5de4fc26454d062f77fcdad0d48))
* **security:** keep active audits parallel ([da1802a](https://github.com/0xPlayerOne/model-gateway/commit/da1802a87882cb509016704b7459b0708b3c4cf5))
* **security:** remove redundant Rust tool cache ([5588abc](https://github.com/0xPlayerOne/model-gateway/commit/5588abc7176f01738b642433043989aa7258fb3e))
* **security:** skip unused Python package cache ([1322bee](https://github.com/0xPlayerOne/model-gateway/commit/1322beee726ba53de5153a686268e1562f38954a))
* **security:** start audits concurrently ([5d5685c](https://github.com/0xPlayerOne/model-gateway/commit/5d5685c19087c343ca3ff3b2227dd007627e5659))
* **setup:** skip duplicate build cache with remote turbo ([5b208d2](https://github.com/0xPlayerOne/model-gateway/commit/5b208d245a8affd97718625ac4d262e39a1803dc))
* **setup:** skip lint cache outside lint jobs ([68533e7](https://github.com/0xPlayerOne/model-gateway/commit/68533e76c7d727bd3565d597ae873dd6a5e1f19a))
* **template:** skip unused formatter caches ([7e1282d](https://github.com/0xPlayerOne/model-gateway/commit/7e1282d6661c090918650a0c9c520c19b5ce8320))
* **test:** benchmark Rust integration cache ([807745d](https://github.com/0xPlayerOne/model-gateway/commit/807745d94ea45ac2cf2293c82e2a963ddcc65a49))
* **test:** cache Rust integration builds ([16934cf](https://github.com/0xPlayerOne/model-gateway/commit/16934cf92175c0b4a27929ed820f3cab73cc68ca))
* **test:** use slim runner for unit checks ([9d557ec](https://github.com/0xPlayerOne/model-gateway/commit/9d557ecdfadf5814ca242ceff94d8806de1d31d0))
* **workflows:** bound automation jobs ([fcdac64](https://github.com/0xPlayerOne/model-gateway/commit/fcdac64a8fd86ed40436162af66b6476a5b8416a))
* **workflows:** bound hosted job time ([0bf4ded](https://github.com/0xPlayerOne/model-gateway/commit/0bf4ded206e6364cab190b53b3817257abbf38b8))
* **workflows:** cancel stale branch runs ([4b6196f](https://github.com/0xPlayerOne/model-gateway/commit/4b6196fdef99c780c8e73fd5ef92f56782a9daa6))
* **workflows:** deduplicate same-commit runs ([a886182](https://github.com/0xPlayerOne/model-gateway/commit/a8861828574ebd2fa04c91b6004445ef4096b19c))


### Reverts

* **ci:** remove ineffective Rust lint cache ([f7ad4df](https://github.com/0xPlayerOne/model-gateway/commit/f7ad4df98d075cfdb6880f4c9d47f1f8977baf64))
* **experiment:** isolate Rust build caches ([b808899](https://github.com/0xPlayerOne/model-gateway/commit/b808899e242e189e9bdacf5b0e771b61b81e062e))

## Changelog

All notable changes to this project are documented here.
