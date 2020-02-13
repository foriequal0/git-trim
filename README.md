[![CI](https://github.com/foriequal0/git-trim/workflows/CI/badge.svg?event=push)](https://github.com/foriequal0/git-trim/actions?query=workflow%3ACI) [![crates.io](https://img.shields.io/crates/v/git-trim.svg)](https://crates.io/crates/git-trim)

git-trim
========

![git-trim Logo](images/logo.png)

`git-trim` automatically trims your git remote tracking branches that are merged or gone.

## Instruction

### Installation
Download binary from [Releases](https://github.com/foriequal0/git-trim/releases), and put it under your `PATH` directories.

You can also install with `cargo install git-trim` if you have `cargo`.

It uses [`git2`](https://crates.io/crates/git2) under the hood which depends conditionally on [`openssl-sys`](https://crates.io/crates/openssl) on *nix platform.
You might need to install `libssl-dev` packages if you build from the source. See: https://docs.rs/openssl/0.10.28/openssl/#automatic

### How to use
1. Don't forget to set an upstream for a branch that you want to trim automatically.
   `git push -u <remote> <branch>` will set an upstream for you on push.
1. Run `git trim` if you need to trim branches especially after PR reviews. It'll automatically recognize merged or gone branches, and delete it.
1. If you need more power, try `git trim --filter all`
1. You can also `git trim --dry-run` when you don't trust me.

## Why have you made this? Show me how it works.

### Git's PR workflow is a little bit tedious as a routine.
There are so many lines of commands to type and many statuses of branches that corresponding to PRs that you've sent.
Were they merged or rejected? Did I forget to delete the remote branch after it is merged?

After some working with the repository, you'll likely to see the mess of merged gone-or-not branches.

![before](images/0-before.png)

They are tedious to remove manually. `git branch --merged` have betrayed us.

![git branch --merged doesn't help](images/1-branch-merged.png)

After the PR is merged or rejected, you're likely to delete them manually if you don't have `git-trim` but it is tedious to type and error-prone.

![old way of deleting them](images/2-old-way.png)

You repeat these same commands as much as PRs that you've sent.
You have to remember what branch is for the PR that just have been closed and it is easy to make a mistake.
I feel nervous whenever I put `--force` flag. Rebase merge forces to me to use `--force` (no pun is intended).
`git reflog` is a fun command to play with, isn't it? Also `git remote update` and `git push` is not instantaneous.
I hate to wait for the prompt even it is a fraction of a second when I have multiple commands to type.

![gvsc before](images/gvsc-0.png)

### See how `git-trim` works!

It is enough to type just `git trim` and hit the `y` key once.

![git trim](images/3-git-trim-in-action.png)

Voila!

![after](images/4-after.png)

That's why I've made `git-trim`.
It knows whether a branch is merged into the default base branch, or whether it is rejected.
It can even `push --delete` when you forgot to delete the remote branch if needed.

![gvsc after](images/gvsc-1.png)

## What kind of merge styles that `git-trim` support?

* A classic merge with a merge commit with `git merge --no-ff`
* A rebase merge with `git merge --ff-only`
* A squash merge with `git merge --squash` (With this method: https://stackoverflow.com/a/56026209)

## What is the difference between the `merged` and `gone` branch?

A merged branch is a branch that you can safely remove them.
It is already merged into the base branch, so you're not going to lose the changes.

However, your PRs are sometimes rejected and deleted from the remote.
Or you might forget the fact that the PR is merged.
So you might have been mistakenly amended or rebased the branch and the patch is now completely different from the patch that is merged.
Then it is `gone`, which means that you might lose your changes. The term is borrowed from the git's remote tracking states.

## Disclaimers
Git and the Git logo are either registered trademarks or trademarks of Software Freedom Conservancy, Inc., corporate home of the Git Project, in the United States and/or other countries.

The logo is a derivative work of [Git Logo](https://git-scm.com/downloads/logos). Git Logo by [Jason Long](https://twitter.com/jasonlong) is licensed under the [Creative Commons Attribution 3.0 Unported License](https://creativecommons.org/licenses/by/3.0/).

Images of a man with heartburn are generated with [https://gvsc.rajephon.dev](https://gvsc.rajephon.dev)
