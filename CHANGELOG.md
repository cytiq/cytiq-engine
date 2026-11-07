# Changelog
## [0.4.1](https://github.com/cytiq/cytiq-engine/compare/0.4.0..0.4.1) - 2026-11-07

### ⛰️  Features

- *(cli)* [**breaking**] Unify exit codes and add JSON outcomes ([#190](https://github.com/cytiq/cytiq-engine/issues/190)) - ([a598042](https://github.com/cytiq/cytiq-engine/commit/a59804252409f82f109f4e9ab700132a415e0785))
- *(execution)* Safely reuse verified outputs and recover interrupted runs ([#193](https://github.com/cytiq/cytiq-engine/issues/193)) - ([55b3ed5](https://github.com/cytiq/cytiq-engine/commit/55b3ed54f0518e241187f4cc62532726a9080a28))
- *(specimen)* Validate reference-ratio anchor reliability ([#191](https://github.com/cytiq/cytiq-engine/issues/191)) - ([fbfbcb3](https://github.com/cytiq/cytiq-engine/commit/fbfbcb3175a007650467f40756df81efce605552))
- Export specimen reports as HTML CSV and JSON ([#187](https://github.com/cytiq/cytiq-engine/issues/187)) - ([2100ac5](https://github.com/cytiq/cytiq-engine/commit/2100ac59cfaade16e520f57198b0506cd62eb0e3))
- Compare native specimen result packages ([#186](https://github.com/cytiq/cytiq-engine/issues/186)) - ([3d10b61](https://github.com/cytiq/cytiq-engine/commit/3d10b613d8335c2c75cc703e247e9beb44c26399))
- Add specimen planning and population reconciliation ([#185](https://github.com/cytiq/cytiq-engine/issues/185)) - ([120825a](https://github.com/cytiq/cytiq-engine/commit/120825a958cfad60f7b5cb2af1eaf53275a472eb))

### 🚜 Refactor

- [**breaking**] Consolidate inspection and preflight crates ([#197](https://github.com/cytiq/cytiq-engine/issues/197)) - ([2fa80b9](https://github.com/cytiq/cytiq-engine/commit/2fa80b94509e55b7ec58b6cff313517947010251))
- Simplify shared inputs, observations, and reports ([#196](https://github.com/cytiq/cytiq-engine/issues/196)) - ([5d3f9ab](https://github.com/cytiq/cytiq-engine/commit/5d3f9ab74d492754c158369b2c802fff32baa067))

### 🧪 Testing

- *(population)* Harden specimen contracts and fuzz coverage ([#192](https://github.com/cytiq/cytiq-engine/issues/192)) - ([6278848](https://github.com/cytiq/cytiq-engine/commit/6278848e63c8314f3213c08cd44746b36acd39c9))

### ⚙️ Miscellaneous Tasks

- *(agents)* Configure Serena and grepai ([#195](https://github.com/cytiq/cytiq-engine/issues/195)) - ([f845dd9](https://github.com/cytiq/cytiq-engine/commit/f845dd95414c9cc4066b5029e91ad1bf155a4352))
- *(skills)* Add token-efficient-coding for Codex and Claude ([#189](https://github.com/cytiq/cytiq-engine/issues/189)) - ([9884fe9](https://github.com/cytiq/cytiq-engine/commit/9884fe9067bb92d2feeb5b574a4391c516b27409))
- Deduplicate shared agent skills ([#194](https://github.com/cytiq/cytiq-engine/issues/194)) - ([7cee617](https://github.com/cytiq/cytiq-engine/commit/7cee617ec0f9c01490fcdecaf4ba493e0430ceb2))
- Pin development and CI tools with mise ([#188](https://github.com/cytiq/cytiq-engine/issues/188)) - ([26e59cb](https://github.com/cytiq/cytiq-engine/commit/26e59cbc03cf873342d2919815cae310f8c507d9))
## [0.4.0](https://github.com/cytiq/cytiq-engine/compare/0.3.1..0.4.0) - 2026-11-05

### ⛰️  Features

- *(release)* Publish all crates to crates.io on release ([#180](https://github.com/cytiq/cytiq-engine/issues/180)) - ([dfc8b66](https://github.com/cytiq/cytiq-engine/commit/dfc8b6667254e1cbfa7bb97cb09351a875382e99))

### 🐛 Bug Fixes

- *(release)* Upgrade cytiq-ci to 0.4.0 ([#183](https://github.com/cytiq/cytiq-engine/issues/183)) - ([162de23](https://github.com/cytiq/cytiq-engine/commit/162de2354c846979f3892128de4da58012acd3ae))

### 🚜 Refactor

- Rename cytiq-cli to cytiq-engine ([#182](https://github.com/cytiq/cytiq-engine/issues/182)) - ([b34a0e4](https://github.com/cytiq/cytiq-engine/commit/b34a0e4705882c6616a53029932e25f7862c762e))

### ⚙️ Miscellaneous Tasks

- *(release)* Update changelog for 0.4.0 ([#184](https://github.com/cytiq/cytiq-engine/issues/184)) - ([a1860e0](https://github.com/cytiq/cytiq-engine/commit/a1860e0b3bc2590cc6351888c8bc4356a2dbafbf))
- *(skills)* Add Pierre trees and diffs for Codex and Claude ([#179](https://github.com/cytiq/cytiq-engine/issues/179)) - ([bb2327c](https://github.com/cytiq/cytiq-engine/commit/bb2327c42398debe8404aaaad1190cb913ca52ee))
## [0.3.1](https://github.com/cytiq/cytiq-engine/compare/0.3.0..0.3.1) - 2026-11-05

### 🐛 Bug Fixes

- *(ci)* Pin release workflows to the 0.2.0 commit ([#176](https://github.com/cytiq/cytiq-engine/issues/176)) - ([fe09ce3](https://github.com/cytiq/cytiq-engine/commit/fe09ce3f2afdf8d32815b587724c021dca2674ac))

### 🚜 Refactor

- Reduce CRAP risk and add targeted mutation tests ([#174](https://github.com/cytiq/cytiq-engine/issues/174)) - ([78bf649](https://github.com/cytiq/cytiq-engine/commit/78bf649570a92381b10e9e880f6b414a70276236))

### ⚙️ Miscellaneous Tasks

- *(release)* Update changelog for 0.3.1 ([#178](https://github.com/cytiq/cytiq-engine/issues/178)) - ([3c9040e](https://github.com/cytiq/cytiq-engine/commit/3c9040e0e0294d7d4ab82be47669c6a060d7761d))
- *(release)* Update changelog for 0.3.1 ([#175](https://github.com/cytiq/cytiq-engine/issues/175)) - ([a4a9685](https://github.com/cytiq/cytiq-engine/commit/a4a96858e9e95222bffefd2f39f939f7352b4b6b))
- Report workspace CRAP risk against a reviewed baseline ([#173](https://github.com/cytiq/cytiq-engine/issues/173)) - ([2972489](https://github.com/cytiq/cytiq-engine/commit/29724893121999052611695e7b3911361d0ea974))
## [0.3.0](https://github.com/cytiq/cytiq-engine/compare/0.2.1..0.3.0) - 2026-11-04

### ⛰️  Features

- Add cytiq stats, histogram plots and meta provenance for CAS ([#171](https://github.com/cytiq/cytiq-engine/issues/171)) - ([dc3915a](https://github.com/cytiq/cytiq-engine/commit/dc3915ae537838512cdee6ada69ac8e861bb6986))

### 🐛 Bug Fixes

- *(parity)* Run parity-real without real data and compare CompensAID on rated channels ([#169](https://github.com/cytiq/cytiq-engine/issues/169)) - ([1485c1d](https://github.com/cytiq/cytiq-engine/commit/1485c1d2a07a453c30bfbd48ae45c46345d40d64))

### ⚡ Performance

- Reduce transform and compensation allocations ([#170](https://github.com/cytiq/cytiq-engine/issues/170)) - ([422b175](https://github.com/cytiq/cytiq-engine/commit/422b175c7c295a88ade618eb95a688c5d46d74c4))

### ⚙️ Miscellaneous Tasks

- *(release)* Update changelog for 0.3.0 ([#172](https://github.com/cytiq/cytiq-engine/issues/172)) - ([807d922](https://github.com/cytiq/cytiq-engine/commit/807d92226cc8bf02853440f0ef50572f027bace3))
## [0.2.1](https://github.com/cytiq/cytiq-engine/compare/0.2.0..0.2.1) - 2026-11-04

### 🐛 Bug Fixes

- Show plotted events as they are, and treat instrument scatter as scatter ([#166](https://github.com/cytiq/cytiq-engine/issues/166)) - ([a317d0e](https://github.com/cytiq/cytiq-engine/commit/a317d0e5847153f0de796bbc5d82b2087d7b6368))

### ⚙️ Miscellaneous Tasks

- *(release)* Update changelog for 0.2.1 ([#168](https://github.com/cytiq/cytiq-engine/issues/168)) - ([29b050e](https://github.com/cytiq/cytiq-engine/commit/29b050eb5ce4fc55c131eac24ddb7089e0f71eef))
## [0.2.0](https://github.com/cytiq/cytiq-engine/compare/0.1.2..0.2.0) - 2026-11-03

### ⛰️  Features

- *(plot)* Add cytiq plot for vector SVG density, scatter and contour plots ([#160](https://github.com/cytiq/cytiq-engine/issues/160)) - ([8d4790f](https://github.com/cytiq/cytiq-engine/commit/8d4790f7d5f07e9ce611983a400b72b474b5866b))

### ⚙️ Miscellaneous Tasks

- *(release)* Update changelog for 0.2.0 ([#164](https://github.com/cytiq/cytiq-engine/issues/164)) - ([ccfc8f0](https://github.com/cytiq/cytiq-engine/commit/ccfc8f05bd7181f130eca66575a90597206204ad))
- Cache Docker publication and measure Rust CI - ([8ae1640](https://github.com/cytiq/cytiq-engine/commit/8ae1640c5a2ff2c2763f4320387df02692428925))

### Build

- *(docker)* Compile Rust dependencies in a cargo-chef layer ([#162](https://github.com/cytiq/cytiq-engine/issues/162)) - ([f8b3718](https://github.com/cytiq/cytiq-engine/commit/f8b3718c935e760131af2588c5a46823e89f71ab))
## [0.1.2](https://github.com/cytiq/cytiq-engine/compare/0.1.1..0.1.2) - 2026-11-02

### ⛰️  Features

- *(clean)* Add --debris-mode off ([#154](https://github.com/cytiq/cytiq-engine/issues/154)) - ([219244f](https://github.com/cytiq/cytiq-engine/commit/219244f8b727ac69e8e367e430af0374e5d101fb))

### ⚙️ Miscellaneous Tasks

- *(release)* Update changelog for 0.1.2 ([#156](https://github.com/cytiq/cytiq-engine/issues/156)) - ([e1954db](https://github.com/cytiq/cytiq-engine/commit/e1954db47234e44a3c4dc84337fdd15464c5fef2))
## [0.1.1](https://github.com/cytiq/cytiq-engine/compare/0.1.0..0.1.1) - 2026-11-02

### ⚙️ Miscellaneous Tasks

- *(release)* Update changelog for 0.1.1 ([#152](https://github.com/cytiq/cytiq-engine/issues/152)) - ([aeb82e0](https://github.com/cytiq/cytiq-engine/commit/aeb82e09383ba1d442ca68b802def327a63dec42))
- Publish the cytiq image for linux/amd64 and linux/arm64 ([#150](https://github.com/cytiq/cytiq-engine/issues/150)) - ([3531252](https://github.com/cytiq/cytiq-engine/commit/35312528fda1b16ea9983aef298ba3e9ead72849))
## [0.1.0] - 2026-11-02

### ⛰️  Features

- Publish every output through one all-or-nothing crate ([#120](https://github.com/cytiq/cytiq-engine/issues/120)) - ([1ca7347](https://github.com/cytiq/cytiq-engine/commit/1ca73479b2ec27290ec5f2cd079d758557627067))
- Add a real-data CytoNorm parity pilot ([#113](https://github.com/cytiq/cytiq-engine/issues/113)) - ([c831886](https://github.com/cytiq/cytiq-engine/commit/c831886f581bc76e0f0a09e4980a4dabe9f7f675))
- Classify CyTOF FCS with the cytof arcsinh recipe ([#112](https://github.com/cytiq/cytiq-engine/issues/112)) - ([1077272](https://github.com/cytiq/cytiq-engine/commit/10772720e9ffe63d7559aec5a54e63dbd288f380))
- Load JSON and YAML classification bundles v4 ([#111](https://github.com/cytiq/cytiq-engine/issues/111)) - ([c90d3da](https://github.com/cytiq/cytiq-engine/commit/c90d3da0b09e7fe1e5e0812ff49233a87967cd35))
- Add CytoNorm batch normalization ([#110](https://github.com/cytiq/cytiq-engine/issues/110)) - ([d389cd5](https://github.com/cytiq/cytiq-engine/commit/d389cd569f3489703470d24d406a1c727fa32eee))
- Support vocabulary-aware classification bundles v2 ([#109](https://github.com/cytiq/cytiq-engine/issues/109)) - ([416d240](https://github.com/cytiq/cytiq-engine/commit/416d240a61bf21b5f6ca2d0702ee44b4ee4c4c1e))
- Benchmark spillqc detection on real data ([#107](https://github.com/cytiq/cytiq-engine/issues/107)) - ([3116015](https://github.com/cytiq/cytiq-engine/commit/31160159ff3d4809df300f5faea4a9dcc857f6ea))
- Add spillqc, CompensAID spillover QC with a batch view ([#105](https://github.com/cytiq/cytiq-engine/issues/105)) - ([89d2329](https://github.com/cytiq/cytiq-engine/commit/89d2329daf58bc52d5963a1d81e0e55f698bf703))
- Add panel-specific ONNX classification ([#91](https://github.com/cytiq/cytiq-engine/issues/91)) - ([3073023](https://github.com/cytiq/cytiq-engine/commit/3073023a83699b54aa372dd1810f3d721bd88a99))
- Declared signal state in the manifest, checked by preflight ([#85](https://github.com/cytiq/cytiq-engine/issues/85)) - ([7b3f7aa](https://github.com/cytiq/cytiq-engine/commit/7b3f7aa996804a711de29886e2faffad7e58eef8))
- Provenance schema v2 with build, config and roles ([#78](https://github.com/cytiq/cytiq-engine/issues/78)) - ([3f422e8](https://github.com/cytiq/cytiq-engine/commit/3f422e81000b0088577f343e0d9c542f8e06f1af))
- Qc stage with panel policies ([#77](https://github.com/cytiq/cytiq-engine/issues/77)) - ([b5fc9e8](https://github.com/cytiq/cytiq-engine/commit/b5fc9e89ad24fad2c6a6c231ddce3a01e4551075))
- Multi-panel manifest and preflight calibration modes ([#76](https://github.com/cytiq/cytiq-engine/issues/76)) - ([6367c50](https://github.com/cytiq/cytiq-engine/commit/6367c504f1226cbcfd6a94dc2c19bc70afe8d30f))
- Preflight, provenance and P1 fixes (clean, cytoscan, autospectral, flowsom) ([#72](https://github.com/cytiq/cytiq-engine/issues/72)) - ([e34ba2c](https://github.com/cytiq/cytiq-engine/commit/e34ba2cac8df477c9e6389e3d3f3595781b2ed7f))
- Compensate and masks ([#71](https://github.com/cytiq/cytiq-engine/issues/71)) - ([c59fce6](https://github.com/cytiq/cytiq-engine/commit/c59fce647ae66413614ba61b44752a9f4ce5914d))
- Add cytiq meta command for FCS metadata export ([#66](https://github.com/cytiq/cytiq-engine/issues/66)) - ([21490ee](https://github.com/cytiq/cytiq-engine/commit/21490ee9c76d8d1513e593853477fea82cdb715b))
- Add cytiq clean stage for debris, doublet and dead-cell removal ([#65](https://github.com/cytiq/cytiq-engine/issues/65)) - ([3639932](https://github.com/cytiq/cytiq-engine/commit/36399325a34db1382474c50fd7cf3cc133f0b1eb))
- Add Gating-ML 2.0 gate evaluation command ([#64](https://github.com/cytiq/cytiq-engine/issues/64)) - ([dadc399](https://github.com/cytiq/cytiq-engine/commit/dadc3994f7aa07d7c072ee7bd042ef92a4d37ee7))
- Add cytoscan anomaly detection command ([#26](https://github.com/cytiq/cytiq-engine/issues/26)) - ([b0a4b3b](https://github.com/cytiq/cytiq-engine/commit/b0a4b3bd81c3bceb419cd8ce4469395f9fe1fd25))
- Add fcs transform command ([#24](https://github.com/cytiq/cytiq-engine/issues/24)) - ([5d2fd04](https://github.com/cytiq/cytiq-engine/commit/5d2fd044e3e8f1f2a332eb2c53e2cc2eee1129e5))
- Add flowsom clustering command ([#23](https://github.com/cytiq/cytiq-engine/issues/23)) - ([ad736ea](https://github.com/cytiq/cytiq-engine/commit/ad736ea175f5db05f750b8876c029be6969ff8aa))
- Add peacoqc workflow ([#20](https://github.com/cytiq/cytiq-engine/issues/20)) - ([9fb8ca9](https://github.com/cytiq/cytiq-engine/commit/9fb8ca913484e83f84bb0e989928e5ed0a682e5e))
- Add autospectral workflow ([#18](https://github.com/cytiq/cytiq-engine/issues/18)) - ([a8cbbec](https://github.com/cytiq/cytiq-engine/commit/a8cbbecf85125c019c4e03b9387edca0ad9e1cbf))
- Add autospill pipeline and CLI ([#11](https://github.com/cytiq/cytiq-engine/issues/11)) - ([5bdf201](https://github.com/cytiq/cytiq-engine/commit/5bdf201daaed545ba249e56044017a57c1e68614))
- Add strict FCS 3.0/3.1/3.2 reader crate ([#7](https://github.com/cytiq/cytiq-engine/issues/7)) - ([87bd9c6](https://github.com/cytiq/cytiq-engine/commit/87bd9c67f62626e6f46c0a74c975f3a4dfc2886c))

### 🐛 Bug Fixes

- Classify an input one batch at a time instead of loading it whole ([#137](https://github.com/cytiq/cytiq-engine/issues/137)) - ([0d9adb5](https://github.com/cytiq/cytiq-engine/commit/0d9adb5fed3e7f086dd499c70975d6f137926c1d))
- Bound decoded DATA and ANALYSIS before reading them ([#135](https://github.com/cytiq/cytiq-engine/issues/135)) - ([0c7fd4a](https://github.com/cytiq/cytiq-engine/commit/0c7fd4a73edf7c82bc91d05d5359e26f111fd196))
- Undo a partial publication when commit fails ([#134](https://github.com/cytiq/cytiq-engine/issues/134)) - ([870c8c6](https://github.com/cytiq/cytiq-engine/commit/870c8c62167ef7b88d685eb2ec75be265cab938d))
- Publish every command's outputs as one set ([#121](https://github.com/cytiq/cytiq-engine/issues/121)) - ([53da846](https://github.com/cytiq/cytiq-engine/commit/53da84624015d0585fc526eb493c8b269bfa9547))
- Classify Kaluza re-exports and validate cytiq on FlowCyt data ([#119](https://github.com/cytiq/cytiq-engine/issues/119)) - ([7ebba74](https://github.com/cytiq/cytiq-engine/commit/7ebba74b69a8b875a42cea24485026c218f1e7e2))
- Bound every size an FCS file declares before acting on it ([#116](https://github.com/cytiq/cytiq-engine/issues/116)) - ([b9788be](https://github.com/cytiq/cytiq-engine/commit/b9788be27d85b4b7987020384b53bdaa2d5f76a0))
- Link compiler-rt on macOS so release builds link again ([#106](https://github.com/cytiq/cytiq-engine/issues/106)) - ([7efe179](https://github.com/cytiq/cytiq-engine/commit/7efe1796850da878984be82236674f329c0d0df9))
- Make classify work after Cargo build without runtime setup ([#92](https://github.com/cytiq/cytiq-engine/issues/92)) - ([3d234c7](https://github.com/cytiq/cytiq-engine/commit/3d234c7572413542717409166d3268ea13f2d441))
- Preflight signal state, QC without policy, eligible coverage ([#90](https://github.com/cytiq/cytiq-engine/issues/90)) - ([bf63c64](https://github.com/cytiq/cytiq-engine/commit/bf63c645b54aecb5f7417d66f0433aef0c922804))
- Refuse log-amplified channels where values are read as stored ([#87](https://github.com/cytiq/cytiq-engine/issues/87)) - ([c29b614](https://github.com/cytiq/cytiq-engine/commit/c29b61455baf4b9133e4d41834badca721b3cdca))
- Raw margin mask without flowCore's -111 floor ([#84](https://github.com/cytiq/cytiq-engine/issues/84)) - ([783a46e](https://github.com/cytiq/cytiq-engine/commit/783a46ebd59e19ba3b00d008399aeb82bf53ec0a))
- Signal-state checks at stage boundaries ([#75](https://github.com/cytiq/cytiq-engine/issues/75)) - ([9557739](https://github.com/cytiq/cytiq-engine/commit/95577394c9e2cadcc1a89477e3977ba879c96f21))
- Acquisition identity for masks, exact event IDs, pre-run provenance ([#74](https://github.com/cytiq/cytiq-engine/issues/74)) - ([aa94a7b](https://github.com/cytiq/cytiq-engine/commit/aa94a7b7d96648fe8374bc16efea34a2ab7fa835))
- FCS data integrity across stages (large files, metadata, margins, lineage) ([#69](https://github.com/cytiq/cytiq-engine/issues/69)) - ([c2f01c4](https://github.com/cytiq/cytiq-engine/commit/c2f01c4819389d974a33100c5932f4a0bbd68ba8))

### 🚜 Refactor

- Split the CLI into one module per command ([#138](https://github.com/cytiq/cytiq-engine/issues/138)) - ([d02bd01](https://github.com/cytiq/cytiq-engine/commit/d02bd015bdda21ede726099928693bb3c25a39fb))
- Parity fixes ([#63](https://github.com/cytiq/cytiq-engine/issues/63)) - ([1834c29](https://github.com/cytiq/cytiq-engine/commit/1834c2990392333921aaf4e617a569c1da6c899b))

### 📚 Documentation

- Streamline root README and consolidate crate documentation ([#100](https://github.com/cytiq/cytiq-engine/issues/100)) - ([49a6f9a](https://github.com/cytiq/cytiq-engine/commit/49a6f9ac29031a4613d854c7c749a76c9030d6bb))
- Add pipeline diagram to README ([#93](https://github.com/cytiq/cytiq-engine/issues/93)) - ([ecf3a65](https://github.com/cytiq/cytiq-engine/commit/ecf3a653042b579f862fb3e4c24467d36ba96409))
- Add readme for agents ([#67](https://github.com/cytiq/cytiq-engine/issues/67)) - ([74f7fde](https://github.com/cytiq/cytiq-engine/commit/74f7fde961c7ad11a54f7ff91b1a5602a09c2ceb))

### 🧪 Testing

- Property-test and fuzz the FCS reader, writer and masks ([#118](https://github.com/cytiq/cytiq-engine/issues/118)) - ([684cf53](https://github.com/cytiq/cytiq-engine/commit/684cf5373683b44add5f3552e851376c8b3c58fb))
- Pin FCS reader and classify outputs with characterization digests ([#115](https://github.com/cytiq/cytiq-engine/issues/115)) - ([a7d5a0a](https://github.com/cytiq/cytiq-engine/commit/a7d5a0a35422acba70a161a5f940215bdc0ca48a))
- Stage 3 scientific validation of the whole chain ([#73](https://github.com/cytiq/cytiq-engine/issues/73)) - ([e1c789d](https://github.com/cytiq/cytiq-engine/commit/e1c789dcb8fb94eebe1690b042e7f84e4325a30f))

### ⚙️ Miscellaneous Tasks

- *(mergify)* Upgrade configuration to current format ([#4](https://github.com/cytiq/cytiq-engine/issues/4)) - ([7437189](https://github.com/cytiq/cytiq-engine/commit/74371899b5c25f9ed2cb528f077a0f8183e7a5ae))
- *(release)* Update changelog for 0.1.0 ([#148](https://github.com/cytiq/cytiq-engine/issues/148)) - ([b822f61](https://github.com/cytiq/cytiq-engine/commit/b822f61c09a0bbf9de22a0876eea5f77c8151364))
- *(repo)* Initialize rust workspace tooling ([#2](https://github.com/cytiq/cytiq-engine/issues/2)) - ([b3e45ad](https://github.com/cytiq/cytiq-engine/commit/b3e45ad45c31cf5c7fe94907f32894b6b3ed3ee0))
- Publish the cytiq image to GHCR on release ([#144](https://github.com/cytiq/cytiq-engine/issues/144)) - ([ebbda0f](https://github.com/cytiq/cytiq-engine/commit/ebbda0f119b42c7919c2d6309c16dea70f05e899))
- Fuzz the FCS reader on a schedule and replay saved crashes in tests ([#136](https://github.com/cytiq/cytiq-engine/issues/136)) - ([6701b24](https://github.com/cytiq/cytiq-engine/commit/6701b2405c833a26b75a6aceccf15cb14d58ce09))
- Run release and Docker builds only when packaging changes ([#124](https://github.com/cytiq/cytiq-engine/issues/124)) - ([4c2845e](https://github.com/cytiq/cytiq-engine/commit/4c2845e142c8ae7aedaa11332c1b3f5f701904c0))

### Build

- Check integer overflow in release builds ([#117](https://github.com/cytiq/cytiq-engine/issues/117)) - ([853a132](https://github.com/cytiq/cytiq-engine/commit/853a1326ecc66aecac91770059c950824f42127a))

### Parity

- FACSCelesta and FACSymphony controls; cap autospill controls at $OnR ([#82](https://github.com/cytiq/cytiq-engine/issues/82)) - ([396070c](https://github.com/cytiq/cytiq-engine/commit/396070c5d8e905324ec42be2872e5d8ca296bce8))
- Pinned, portable local suites and real-data registry ([#79](https://github.com/cytiq/cytiq-engine/issues/79)) - ([428c173](https://github.com/cytiq/cytiq-engine/commit/428c173e10869f74e7d0851bb74badfd22bef06f))
<!-- generated by git-cliff -->
