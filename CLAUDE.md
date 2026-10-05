# hayai — agent instructions

- Task tracking: `bd` (beads). Run `bd create` for an issue at the start of multi-step work. Run `bd close` when the work is done. Never commit, push or `bd dolt push`: the repository owner commits.
- Before a commit is proposed, the pre-commit gate must pass: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. The gate is the full run: it has the comparisons with Zakura and Zebra, hayai-fuzz and the slow tests.
- The CI of each push (`.github/workflows/ci.yml`) runs a smaller set: `cargo test --workspace --exclude hayai-fuzz --no-default-features --features upstream --release -- --skip slow::`, and clippy with the same package set and features. This set builds no zakura-* crate and no zebra-chain.
- A test that costs much and that a push does not need goes into a module named `slow` (`mod slow { use super::*; ... }`). No other test path can contain `slow::`.
- `CHANGES.md` records design decisions and lessons (short sections, behaviour-level). `CHANGELOG.md` records user-visible changes in one line each.
- Rust style: use pattern matching (`let Some(x) = .. else`, `match`, `matches!`) instead of `.is_some()/.is_none()/.is_ok()/.is_err()`. No speculative abstractions. No dangling code. Every feature has a test.
- Consensus-critical code never silently skips a check. It must return an error.
- Code reaches cryptographic primitives through the `hayai-crypto` facade. The facade re-exports the upstream Zcash crates by default and the `zakura-*` forks under its `zakura` feature. Crates never name `orchard`, `zcash_primitives`, `pasta_curves`, ... directly, so both backends build. `hayai-bench` additionally depends on the `zakura-*` crates and on `zebra-chain` as the comparison baselines, behind its default feature `baselines`. The Zakura backend with the baselines is `--no-default-features --features zakura,baselines`.

## Containers

- Never start a container with `--privileged`, with `--pid=host`, or with a bind mount of the host `/dev`. On 2026-10-03 a privileged systemd container started `getty` on the host `tty1` and ended the desktop session of the owner.
- A test of the systemd unit needs a systemd container. Use this set, and no more: `--cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw --tmpfs /run --tmpfs /run/lock --cap-add SYS_ADMIN`.
- The image of such a container must set `ENV container=docker` and must mask the terminal units: `systemctl mask getty@.service serial-getty@.service console-getty.service getty-static.service getty.target`.
- Remove each container and each image that a check starts, by name, when the check ends. Never use `docker ... prune`.
