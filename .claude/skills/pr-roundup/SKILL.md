---
name: pr-roundup
description: Tag pull requests in pr-roundup so related work is grouped for review. Use after opening or pushing a pull request, when asked to tag, group or organize pull requests, or when asked which pull requests belong together.
---

# Tagging pull requests with pr-roundup

pr-roundup lists the user's open pull requests on a local page, grouped by a
tag each one is given, and copies each group into Slack so it can be reviewed
together. A pull request has at most one tag. Tag freely: the user doesn't
need to be asked first.

## Commands

```sh
pr-roundup tags list                  # every tag and its pull requests
pr-roundup tags set <PR> "<tag>"      # set (or replace) a pull request's tag
pr-roundup tags unset <PR>            # remove it
```

`<PR>` is a GitHub URL or `owner/repo#123`. The commands work whether or not
the pr-roundup server is running; a running one updates its page.

## Choosing a tag

A tag names a piece of work that a reviewer would want to read as one
meaningful chunk: the pull requests that only make sense next to each other,
or that together finish one goal.

- **Run `pr-roundup tags list` first** and reuse an existing tag when the pull
  request belongs to that work, even across repositories. Don't create a
  near-duplicate of a tag that already exists.
- **Not too broad.** A tag like "fixes", "backend" or "misc" lumps unrelated
  work together, and nobody can review it as one thing.
- **Keep a ticket's and a stack's pull requests together.** Pull requests
  linked to the same Jira or Linear ticket almost always share a tag, as do
  the pull requests in one GitHub stack (each based on the one before). Split
  them only when there are a lot of them, or the ticket is scoped too broadly
  to review as one thing.
- **Not too narrow.** A tag usually spans several tickets or stacks that
  serve the same goal; a tag per pull request, per ticket or per stack groups
  too little. A single pull request gets its own tag only when it really is
  its own piece of work and more is expected to join it.
- **A tag is the headline reviewers see in Slack**, above the titles of its
  pull requests, which they see too. It groups work that is already named, so
  it doesn't repeat what the titles say. Keep it short (one to ten words) and
  only as specific as it needs to be to tell the work apart from the other
  tagged groups that have open pull requests. Name the work in plain
  lowercase words, not a ticket number or a branch name.
- **Sharpen tags as the work grows.** A broad tag can be right while it is
  the only group of its kind. When a new pull request would blur it, for
  example alerting work landing next to an "observability" group of logging
  and metrics changes that alerting doesn't use, propose to the user splitting
  it into narrower tags ("alerting" for the work that builds towards alerts,
  "observability" for the rest). Ask before retagging pull requests that
  already have a tag; tagging untagged ones needs no asking.
- When a new pull request continues work that is tagged, give it that tag.
  When it starts a new effort, start a new tag, and say so in your reply so the
  user can rename or merge it.
