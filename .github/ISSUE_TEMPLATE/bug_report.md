---
name: Bug report
about: Create a report to help us improve
title: ''
labels: ''
assignees: ''

---

**Check your version before submitting the bug**
`git-trim` is still `0.x` version and I do make a lot of silly bugs.
Some bugs might be fixed on upstream version. Please update it and make sure that you're using the upstream version
especially you've installed `git-trim` other than `cargo install` such as Homebrew or AUR.

**Describe the bug**
A clear and concise description of what the bug is.

**To Reproduce**
1. Minimal reproducible git repo if available
2. CLI command and configs
3. Steps to reproduce the behavior

**Expected behavior**
A clear and concise description of what you expected to happen.

**Actual behaviour**
If applicable, add logs and stacktraces to help explain your problem.

**Additional context and logs & dumps if necessary**
You should remove sensitive informations before put them here.
 - OS
 - Version
 - `git rev-parse --abbrev-ref HEAD`
 - `git show-ref`
 - `git config --get-regexp '(push|fetch|remote|branch|trim).*' | sort`
 - `git log --oneline --graph --all`

**Logs and stacktraces if necessary**
You should remove sensitive informations before put them here.
You can get more detailed and clean logs by setting some environment variable with follwing command
```shell
export RUST_LOG=trace
export RAYON_NUM_THREADS=1
export RUST_BACKTRACE=full
git trim
```

```
Put them here
```
