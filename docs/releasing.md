# Releasing

Releases go through `.github/workflows/cd.yml`, which calls the reusable changelog and Docker workflows of [`cytiq-ci`](https://github.com/cytiq/cytiq-ci).
1. Pushing an `X.Y.Z-rcN` tag opens a `chore/update-changelog-X.Y.Z` PR with `CHANGELOG.md`, `RELEASE_NOTES.md` (git-cliff, `cliff.toml`) and the workspace version bump in `Cargo.toml`/`Cargo.lock`. It also publishes `ghcr.io/cytiq/cytiq-engine:X.Y.Z-rcN` for `linux/amd64` and `linux/arm64`, each built on a native runner.
2. Merging that PR validates the merge commit, publishes the `:X.Y.Z` and `:latest` images from it, then creates the signed tag and GitHub Release.
3. After first-time registration, dispatch `.github/workflows/crates-publish.yml` on the stable tag (see [crates.io publication](#cratesio-publication)).

Images record that commit in `CYTIQ_BUILD_COMMIT` and carry SBOM and provenance attestations. The release jobs need the `GPG_PRIVATE_KEY`, `GPG_PASSPHRASE`, `GPG_FINGERPRINT` and `GH_TOKEN` secrets.

Full scientific validation checks out the resolved release commit before any
publication. The local release job signs that exact commit and refuses to move
an existing version tag. A later change to `main` cannot enter a release that
has already passed validation.

Docker publication opts into the platform-specific registry layer cache in `cytiq-ci` 0.4.0. Cache export failures
do not hide build failures or block a successful publication. `docker/Dockerfile` compiles the dependencies with
cargo-chef in a layer of their own, keyed by a recipe of the manifests and lockfile with the workspace version masked.
Source changes and release version bumps reuse that layer; any dependency change in `Cargo.lock` rebuilds it. The
workspace crates and the fat-LTO link of `cytiq` always rebuild. The `package.yml` job reads the amd64 release cache
and never writes it. Keep the reusable workflow pinned to a released tag after rollout validation.
The changelog and Docker calls use `cytiq-ci@0.6.0`. The image revision label records the checked-out commit.
If GitHub cannot resolve an annotated release tag, pin its peeled commit SHA
and retain the version in a comment; this keeps the same reviewed workflow
code without rewriting the upstream release tag.

## crates.io publication

Every workspace crate is published, because `cytiq-engine` depends on all the
others. They share the workspace version. Library crates are depended on through
`[workspace.dependencies]` in the root `Cargo.toml`, which gives crates.io the
version and local builds the path. Each such version must equal the workspace
version; `just package-check` and the `quality` job reject a mismatch, a crate
without `description` or `license`, and an internal dependency missing from
`[workspace.dependencies]`. The `cytiq-ci` release job pinned in `cd.yml`
bumps `[workspace.package]` and path-dependency requirements on the previous
version together, before running `cargo update --workspace`. The changelog PR
includes the modified manifests and lockfile; external dependency versions stay
locked. This order also supports minor and major releases: a `0.3.1` requirement
cannot resolve a workspace crate after it moves to `0.4.0`.

`.github/scripts/publish_crates.py` checks the versions against the tag, skips
versions already on crates.io and runs one `cargo publish --workspace`, which
orders the crates by their dependencies. The workflow verifies every package by
building it from its `.crate` archive first, then mints a 30-minute token through
trusted publishing (OIDC) and uploads without building again. A failed run is
safe to re-run, or to dispatch by hand on the tag (**Run workflow**, tag
`X.Y.Z`). Compare the tag commit with the image revision label before dispatching.

Trusted publishing cannot create a crate. The first version of a crate, and of
any crate added to the workspace later, is published once by hand. The
workflow refuses a release with such a crate before uploading anything:

1. Sign in to crates.io with GitHub, verify the email and create an API token
   with the `publish-new` and `publish-update` scopes, expiring soon.
2. From a clean checkout of the release tag, `cargo login`, then
   `just publish-crates-first`. It verifies every new crate, then publishes
   them. crates.io creates at most five new crates at once and then about one
   every ten minutes, so the first publication of all 24 takes over three
   hours; the script waits out each refusal and resumes. Ask help@crates.io to
   raise the limit beforehand to shorten it. Revoke the token afterwards.
3. For each crate, add a trusted publisher in **Settings → Trusted Publishing**:
   owner `cytiq`, repository `cytiq-engine`, workflow `crates-publish.yml`,
   environment `crates-io`.

The repository needs a `crates-io` environment (**Settings → Environments**)
whose deployment rule admits only release tags (`[0-9]*.[0-9]*.[0-9]*`; the
workflow itself rejects RC tags); required reviewers are optional. No registry secret is stored.
