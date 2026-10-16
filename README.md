# gate

gate runs a code review between AI agents before you see the change. One agent writes the change on its own branch. Reviewer agents check it against your rules and leave comments. The writer fixes them or argues back, and an arbiter agent settles arguments. You read the result in `gate-gui` and merge it or send it back.

## Install

```sh
cargo install --path .
cargo install --path gui
mkdir -p ~/.gate/reviewers && ln -s "$PWD"/reviewers/*.md ~/.gate/reviewers/
```

These commands link to this checkout instead of copying, so `git pull` gets you the latest reviewers and skill. Run the two `cargo install` lines again after pulling to update the programs.

## Reviewers

A reviewer is a markdown file of rules. gate names it after the file, so `~/.gate/reviewers/naming.md` is the reviewer `naming`. Put rules you want everywhere in `~/.gate/reviewers`, and rules for one project in `.gate/reviewers` in that repo. If both have a file with the same name, the repo's wins. This repo comes with three to start from, linked in by the install step: `arch-correctness`, `arch-simplicity` and `arch-topology`.

## Using it with Claude Code

```sh
ln -s "$PWD"/claude/skills/gate ~/.claude/skills/gate
ln -s "$PWD"/claude/agents/gate-arbiter.md ~/.claude/agents/
```

In any git repo, type `/gate fix-login Fix the crash on the login screen`. Claude writes the change, starts a reviewer for each rules file and an arbiter when needed, and keeps going until you merge or close it in `gate-gui`.

## Using it with another tool

Start from `claude/skills/gate/SKILL.md`. Everything in it is a `gate` command except two Claude Code steps: running `gate wait` with no time limit, and starting each reviewer and the arbiter as a background agent. Swap those for your tool's way of doing the same.
