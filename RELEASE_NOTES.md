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
