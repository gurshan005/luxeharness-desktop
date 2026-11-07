# LuxeHarness

LuxeHarness is a native macOS workspace for coding agents, written in Rust with Iced. A developer can open a project, start a chat, review an agent's proposed actions, and switch folders while the original run continues in its own workspace.

## What to try

1. Install the Xcode Command Line Tools, Rust 1.88 or newer, Git, and ripgrep. From the repository root, run `cargo run --locked`.
2. Choose **Workspace setup → Open folder** to inspect a project. Its working directory, chats, session IDs, and run history stay associated when you switch projects or restart the app.
3. In **Agent control**, select **Check workspace** to inspect local tools before connecting a model. To run an agent, open **Connections** and select an installed Codex or Claude Code CLI, or configure an API endpoint and its credential environment variable. Each developer uses their own provider account.

To build the unsigned macOS app bundle, run `./package-macos.sh` with ripgrep on your PATH. The bundle is written to `dist/LuxeHarness.app`.

## How it works

| Area | Implementation |
| --- | --- |
| Project state | The [workspace registry](src/workspace.rs) stores canonical folder paths, checks for stale writes when saving project guidance, and keeps drafts and connection preferences per project. |
| Provider sessions | [Native transports](src/harness/native.rs) connect to Codex app-server and Claude Code. The [API adapter](src/harness/provider.rs) supports streaming model responses for direct API connections. |
| Concurrent editing | An editing run takes exclusive access to a shared workspace. Separate Git worktrees let multiple agents edit independently and keep each result available for review. |
| Action review | [Permission checks](src/harness/permissions.rs) distinguish plan, ask, and build modes. Shell and external tool actions surface approval requests. The [macOS sandbox](src/harness/sandbox.rs) restricts API shell operations. |
| Recovery | The [run store](src/harness/store.rs) persists events and session records. After a restart, interrupted runs await explicit resumption. [Replay](src/harness/replay.rs) lets a developer inspect a recorded API session without executing its tool proposals. |

## Verification

```sh
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
```

The default suite runs 60 tests, including workspace switching, stale file writes, approval handling, session resumption, and worktree isolation. Tests use temporary projects and local protocol fixtures. Two opt-in tests exercise signed-in provider accounts and are skipped by default.

## Credits

The implementation draws on [Harness Engineering 101](https://isuruwijesiri.com/harness-engineering-101/) by Isuru Wijesiri. Geist is distributed under the [SIL Open Font License](assets/fonts/Geist-OFL.txt). macOS packages include the [ripgrep MIT notice](assets/ripgrep-LICENSE-MIT).
