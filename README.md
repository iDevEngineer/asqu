# Asqu

**Version control that nobody has to manage. Ask. See. Undo.**

Asqu keeps a full history of your project by itself, in plain words. It works the
same whether the changes come from you or from any AI tool. You can see what changed,
go back to "Tuesday" or "before the signup change", and try ideas side by side, with
no branches, commits or merges to learn.

## You don't need to be technical

If you build with an AI tool, you never have to type an Asqu command. Just ask in plain words:

- "Install Asqu and set it up for this project."
- "What changed today?"
- "Undo that last change."
- "Go back to how it was on Tuesday."
- "Try this two different ways and show me which is better."

Your AI runs Asqu for you. Asqu keeps the history, so nothing is ever lost, and anything that needs a real decision, like approving a risky change, is always left to you.

## Install

macOS and Linux:

```sh
curl -fsSL https://asqu.dev/install | sh
```

Windows (PowerShell):

```powershell
irm https://asqu.dev/install.ps1 | iex
```

Then, in a project folder:

```sh
asqu watch      # save automatically whenever files change
asqu log        # see the Timeline
asqu undo       # go back one change (can itself be undone)
asqu open       # the Asqu view in your browser
```

Run `asqu --help` for everything else, or `sh demo.sh` for a 5-minute guided tour.

## What it does

- **Saves by itself.** Every change goes on a Timeline, labelled with the request that caused it.
- **Undo in plain words.** `asqu back "2 hours ago"`, `asqu back "#3"`, `asqu undo 2`.
- **Tries.** Separate attempts (by you or several AIs at once) that you keep or toss. Asqu compares them and suggests a winner; you choose.
- **Works with any AI tool.** `asqu setup` adds a note to AGENTS.md; `asqu tools` gives any model Asqu's actions as tool definitions; `asqu serve` gives a local web API.
- **Signed checks.** `asqu check` runs your tests and signs a receipt for the exact files that passed; `asqu release` only releases those files.
- **Your work stays yours.** History lives on your computer and in backup places you choose (a folder, iCloud Drive, Dropbox, a disk, or S3-style storage). Your work is never sent to us.
- **Starts and leaves cleanly.** There is no set-up step: a history starts in a project's folder the first time something is saved there (looking, as with `asqu status`, never starts one, and your home folder is never treated as a project). `asqu remove` takes Asqu out of a folder again: the history is moved aside, not deleted, your files are untouched, and only you can do it, never an AI.
- **AsquHub (optional).** `asqu link hub` shares a project on [AsquHub](https://asquhub.com), the hosted home for people and AI agents working together.

## Already using Git?

You don't have to switch, and nothing in your project changes. Asqu works in the same folder, alongside Git:

- **Your `.gitignore` already works.** Asqu skips the same files Git does. Add an `.asquignore` (same format) only if you want Asqu to skip more.
- **Git never sees Asqu.** Asqu's history lives in a `.asqu` folder that it keeps out of `git status` by itself, without touching your `.gitignore`, so nothing changes for teammates.
- **Keep committing and pushing as before**, or stop when you no longer need to. Asqu's Timeline starts from the day you first use it; your earlier Git history stays in Git.
- **A Git worktree is a Try.** A Git worktree is a second working folder of the same project, which AI tools often make to work in. Asqu records the work in any worktree as a Try of the main project by itself, starting the main project's history if there isn't one yet. `asqu try keep` combines it, with no branch, pull request or merge, and `asqu try update` brings in what changed in the main project meanwhile. Asqu never removes the worktree's folder or touches its Git branch; if the folder is removed, Asqu still has its work as last saved. Undo and going back are run from the main project's folder. A worktree that already had its own Asqu history stays a separate project, as does one that holds only part of the project.

## Build from source

Needs [Rust](https://rustup.rs).

```sh
cargo build --release
cargo test
```

## Licence

Asqu is [Fair Source](https://fair.io) under the [Functional Source License 1.1, Apache 2.0 future licence](LICENSE) (FSL-1.1-ALv2).

In plain words: you may use, read, change and share Asqu freely, for personal or work use. You may not sell it, or a product built from it, as a rival to Asqu or AsquHub. Two years after each version is released, that version is also available under the Apache License 2.0, with no such limit.

AsquHub's server is a separate, closed product.

## Questions and contact

- **Found a bug?** [Open an issue](https://github.com/iDevEngineer/asqu/issues).
- **A question or an idea?** Start a [discussion](https://github.com/iDevEngineer/asqu/discussions).
- **Found a security problem?** Please don't post it publicly. Report it privately: see [SECURITY.md](SECURITY.md).
- **No GitHub account, or a business or licence question?** Use [asqu.dev/contact](https://asqu.dev/contact).

Made by Scott Mackey · [scottmackey.uk](https://scottmackey.uk) · [asqu.dev](https://asqu.dev)
