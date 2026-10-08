# hayai — agent instructions

- Task tracking: `bd` (beads). Run `bd create` for an issue at the start of multi-step work. Run `bd close` when the work is done. Never commit, push or `bd dolt push`: the repository owner commits.
- During work, run only what the change touches: `cargo fmt --all --check`, clippy on the changed crates, and the tests that the change adds or changes plus the test targets of the changed files. Do not run the full suite for each change: it takes too long. CI runs the wide set on each push.
- The full gate (`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --release` on both backends, with the comparisons with Zakura and Zebra, hayai-fuzz and the slow tests) runs once before a release or when the owner asks for it.
- The CI of each push (`.github/workflows/ci.yml`) runs a smaller set: `cargo test --workspace --exclude hayai-fuzz --no-default-features --features upstream --release -- --skip slow::`, and clippy with the same package set and features. This set builds no zakura-* crate and no zebra-chain.
- A test that costs much and that a push does not need goes into a module named `slow` (`mod slow { use super::*; ... }`). No other test path can contain `slow::`.
- `CHANGES.md` records design decisions and lessons (short sections, behaviour-level). `CHANGELOG.md` records user-visible changes in one line each.
- Rust style: use pattern matching (`let Some(x) = .. else`, `match`, `matches!`) instead of `.is_some()/.is_none()/.is_ok()/.is_err()`. No speculative abstractions. No dangling code. Every feature has a test.
- Consensus-critical code never silently skips a check. It must return an error.
- hayai is a performance-first implementation. Every hot path uses the best method that measurement supports:
  - batched reads and writes, no read-before-write;
  - work in parallel off the critical path, with bounded queues;
  - preallocated and reused buffers;
  - fixed-size keys with locality;
  - a stated bound for each in-memory structure.

  No vector that grows for the life of the process, no unbounded queue, no random access without a measured reason. Measure before and after. Never adopt a method on a claim.
- Code reaches cryptographic primitives through the `hayai-crypto` facade. The facade re-exports the upstream Zcash crates by default and the `zakura-*` forks under its `zakura` feature. Crates never name `orchard`, `zcash_primitives`, `pasta_curves`, ... directly, so both backends build. `hayai-bench` also depends on the `zakura-*` crates and on `zebra-chain` as the comparison baselines, behind its default feature `baselines`. The Zakura backend with the baselines is `--no-default-features --features zakura,baselines`.

- `hayai-consensus-core` is the code that Lean proofs will cover. Every change to it keeps the rules of `docs/formal-verification.md`, section "Design rules of the core". Consensus rules go into the core; the other crates only fetch data, run the cryptography and call the core.

## Branch modular-crates: how to continue

The work of the epic "Modular crates" (bd `hayai-7yq`) moved from another machine at commit `7bc096d`. When the owner asks to continue this branch:

1. Read `CHANGES.md`, `docs/formal-verification.md` and `docs/architecture.md`.
2. Run `bd show hayai-7yq` for the open items. If `bd` does not list the issues, import `.beads/issues.jsonl`.
3. Start with M2 (`bd show hayai-rlg`): its hand-off note gives the design of stage 2 of the core. The first step is the removal of `#![no_std]` and `extern crate alloc` from `crates/hayai-consensus-core/src/lib.rs`.
4. Then M14 (Charon and Aeneas extraction of the core), then the other open items. M13 (the performance gate) comes last.
5. The tests did not run on `7bc096d`. Run the tests of the changed crates before the first new commit, and report a failure that the change does not explain to the owner.
6. Ask the owner the open decision of `docs/formal-verification.md` (checkpoint path) before M2 changes `apply_checkpointed`.

Remove this section when the epic merges into `main`.

## Containers

- Never start a container with `--privileged`, with `--pid=host`, or with a bind mount of the host `/dev`. On 2026-10-03 a privileged systemd container started `getty` on the host `tty1` and ended the desktop session of the owner.
- A test of the systemd unit needs a systemd container. Use this set, and no more: `--cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw --tmpfs /run --tmpfs /run/lock --cap-add SYS_ADMIN`.
- The image of such a container must set `ENV container=docker` and must mask the terminal units: `systemctl mask getty@.service serial-getty@.service console-getty.service getty-static.service getty.target`.
- Remove each container and each image that a check starts, by name, when the check ends. Never use `docker ... prune`.
