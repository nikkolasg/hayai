# hayai — agent instructions

- Task tracking: `bd` (beads). Run `bd create` for an issue at the start of multi-step work. Run `bd close` when the work is done. Never commit, push or `bd dolt push`: the repository owner commits.
- During work, run only what the change touches: `cargo fmt --all --check`, clippy on the changed crates, and the tests that the change adds or changes plus the test targets of the changed files. Do not run the full suite for each change: it takes too long. CI runs the wide set on each push.
- The full gate (`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --release` on both backends, with the comparisons with Zakura and Zebra, hayai-fuzz and the slow tests) runs once before a release or when the owner asks for it.
- The CI of each push (`.github/workflows/ci.yml`) runs a smaller set: `cargo test --workspace --exclude hayai-fuzz --no-default-features --features upstream --release -- --skip slow::`, and clippy with the same package set and features. This set builds no zakura-* crate and no zebra-chain. It also runs `scripts/layering.sh`, the crate layering guard: run it after a change of a `Cargo.toml` or of a `use` line.
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

- `hayai-consensus-core` is the code that the Lean proofs of `formal/` cover. Every change to it keeps the rules of `docs/formal-verification.md`, section "Design rules of the core", stays in the Aeneas subset (`formal/README.md`) and regenerates `formal/Hayai/Core` with `formal/scripts/extract.sh`. Consensus rules go into the core; the other crates only fetch data, run the cryptography and call the core.

## Branch formal-core: how to continue

The formal verification of the consensus core (bd `hayai-7yq.1`, then the stages of `docs/formal-verification.md`, section "Decisions", entry "Resume"). When the owner asks to continue:

1. Read `formal/README.md`, `docs/formal-verification.md` and the section "Formal verification, stage A" of `CHANGES.md`.
2. After each change of `crates/hayai-consensus-core`: `formal/scripts/extract.sh` (Charon and Aeneas on PATH, at the commits of `formal/TOOLCHAIN`; on this machine under `~/prog/formal-tools/aeneas/bin` and `~/prog/formal-tools/aeneas/charon/bin`), then `lake build` in `formal/` (`~/.elan/bin`). Commit the regenerated `formal/Hayai/Core`. A new axiom in `FunsExternal_Template.lean` gets a definition in `FunsExternal.lean`.
3. The core stays in the Aeneas subset (`formal/README.md`, section "The Rust subset"). A refusal of Aeneas is reduced to a probe crate first (`charon rustc --preset=aeneas -- --crate-type=lib probe.rs`, then `aeneas probe.llbc -backend lean -dest out`), then fixed in the core.
4. Stage B next: `formal/Hayai/Spec/Header.lean` and `Difficulty.lean` from §7.6 and §7.7 of the protocol specification (not from the code), then `formal/Hayai/Proofs/` with the `progress` tactic of Aeneas. The CI jobs `formal` and `formal-extract` were written without a run: the first push of the branch checks them.
5. On macOS: `hayai-bench` builds, but its tests that read `/proc` and the `hayai-net` and node tests that bind `127.0.0.x` fail there; they pass on Linux. `cargo` needs `LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib` for RocksDB. GNU make (`gmake`) builds Aeneas.

Remove this section when the branch merges into `main`.

## Containers

- Never start a container with `--privileged`, with `--pid=host`, or with a bind mount of the host `/dev`. On 2026-10-03 a privileged systemd container started `getty` on the host `tty1` and ended the desktop session of the owner.
- A test of the systemd unit needs a systemd container. Use this set, and no more: `--cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw --tmpfs /run --tmpfs /run/lock --cap-add SYS_ADMIN`.
- The image of such a container must set `ENV container=docker` and must mask the terminal units: `systemctl mask getty@.service serial-getty@.service console-getty.service getty-static.service getty.target`.
- Remove each container and each image that a check starts, by name, when the check ends. Never use `docker ... prune`.
