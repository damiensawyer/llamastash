# Release 0.0.1 checklist

This is the short go/no-go list for cutting `v0.0.1`. Use it with [`docs/runbooks/release-0.0.1-bootstrap.md`](release-0.0.1-bootstrap.md): the bootstrap runbook covers one-time org and secret setup, while this file is the release-day checklist.

Companion references:

- Bootstrap runbook — [`docs/runbooks/release-0.0.1-bootstrap.md`](release-0.0.1-bootstrap.md)
- Hardware UAT guide — [`docs/testing/hardware-uat.md`](../testing/hardware-uat.md)
- Proxy real-client smoke — [`tests/proxy_real_client_smoke.md`](../../tests/proxy_real_client_smoke.md)
- Release workflow — [`.github/workflows/release.yml`](../../.github/workflows/release.yml)
- Pre-tag guards — [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)

## 0. Stop conditions

- Do not push the release tag while any item below is still red.
- If the tag is already pushed and `publish-cargo` has succeeded, do not cut a second tag to paper over a partial release. Re-run or fix the failed downstream job on the existing run.

---

## 1. Release inputs are final

```sh
git status --short
grep '^version' Cargo.toml
grep -E '^## \[0\.0\.1\]' CHANGELOG.md
```

Pass:

- Working tree is clean, or only contains intentional release-prep edits.
- `Cargo.toml` says `0.0.1`.
- `CHANGELOG.md` has `## [0.0.1] — 2026-05-26`.
- Release-facing docs are in sync with shipped behavior: `README.md`, `INSTALL.md`, `docs/usage.md`, `docs/architecture.md`, and `AGENTS.md`.
- R1 blockers in `TODO.md` are closed or explicitly deferred out of `v0.0.1`.

---

## 2. Quality gates are green

```sh
make fmt-check
make lint
make test
cargo test --features test-fixtures,uat --workspace --no-fail-fast
cargo build --release
```

If the audit toolchain is installed, also run:

```sh
make audit
```

Pass:

- Local fmt, clippy, and test passes are green.
- Feature-enabled UAT tests still pass.
- Release build succeeds without `--features uat`.
- GitHub Actions `ci.yml` is green on `main`, especially `release readiness`, `actionlint`, `install-sh-lint`, and `install-sh-test`.

---

## 3. Product smoke is done

Run the release-path smokes that matter for `v0.0.1`:

1. Manual UAT on the hardware you actually have, following [`docs/testing/hardware-uat.md`](../testing/hardware-uat.md).
2. Proxy smoke with a real client, following [`tests/proxy_real_client_smoke.md`](../../tests/proxy_real_client_smoke.md).
3. One last local CLI/TUI sanity pass: daemon starts, list/status work, and a model can be launched and stopped.

Pass:

- CPU-only release-gate coverage exists in CI.
- Available manual UAT lanes have been run recently enough to trust the release.
- OpenAI-compatible proxy behavior has been checked with a real client, not just tests.

---

## 4. Release infrastructure is ready

```sh
gh auth status
gh secret list --repo llamastash/llamastash
gh api /repos/llamastash/llamastash.github.io/pages --jq '.build_type'
```

Pass:

- `gh` is authenticated as an org owner or maintainer with release rights.
- `CRATES_IO_TOKEN` and `GH_BUMP_TOKEN` are present on `llamastash/llamastash`.
- GitHub Pages for `llamastash.github.io` is configured for the Actions workflow source.
- The bootstrap dry run with `v0.0.0-rc1` has succeeded at least once.

---

## 5. Tag, watch, verify

```sh
git tag v0.0.1
git push origin v0.0.1
```

Watch the release workflow to completion:

```sh
gh run list --repo llamastash/llamastash --workflow=release.yml --limit 1 \
  --json databaseId --jq '.[0].databaseId' \
  | xargs -I {} gh run watch --repo llamastash/llamastash --exit-status {}
```

Then verify every channel:

```sh
gh release view v0.0.1 --repo llamastash/llamastash --json assets
gh api /repos/llamastash/homebrew-llamastash/commits/main --jq '.commit.message'
gh api /repos/llamastash/llamastash.github.io/commits/main --jq '.commit.message'
```

Pass:

- `release.yml` finishes green.
- GitHub Release contains 10 target tarballs, 10 `.sha256` sidecars, `SHA256SUMS`, `install.sh`, and `install.sh.sha256`.
- Homebrew tap `main` has the `v0.0.1` bump commit.
- Marketing site `main` has the `v0.0.1` update commit.
- crates.io shows `llamastash 0.0.1` published.

---

## 6. Fresh-box install smoke

Minimum release-channel check after publish:

1. Ubuntu: `curl -fsSL https://llamastash.dev/install.sh | sh`
2. macOS: `brew install llamastash/llamastash/llamastash`
3. Cargo: `cargo install --locked llamastash --version 0.0.1`

Pass:

- All three install paths produce a working `llamastash --version` on a clean machine or VM/container.
- `https://llamastash.dev/install.sh` serves the expected current script.

---

## 7. Post-release cleanup

Pass:

- Branch protection from the bootstrap runbook is enabled if this was the first real tag.
- Any remaining `v0.0.1` follow-ups stay in `TODO.md` under R2, not in your head.
- Release announcement work starts only after the shipped artifacts have been verified.
