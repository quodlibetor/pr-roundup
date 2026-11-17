# pr-roundup

A local web page that lists your open pull requests, lets you tag them, and
copies each tag's pull requests to the clipboard as a list for Slack, in the
shape `jj spr list --format slack-links --copy` produces.

- Tagging a pull request moves it into that tag's section. A pull request has
  at most one tag; clearing the field (or `×`) removes it.
- A tagged pull request stays in its section after it merges or closes, until
  the tag is removed. Untagged ones disappear when they stop being open.
- Within a section, pull requests are grouped by repository, and each
  repository's pull requests into stacks: GitHub's own stacks first, then
  chains where one pull request's base branch is another's head. Drag a
  repository's heading to reorder repositories within the section.
- GitHub is polled when the server starts and when the page is opened or
  regains focus, at most once per `min_refresh_secs` (a minute by default);
  "Refresh now" polls regardless. Results are pushed to open tabs over
  server-sent events. A live update waits while you are typing a tag.

## Running

```sh
pr-roundup --example-config > ~/.config/pr-roundup/config.toml
$EDITOR ~/.config/pr-roundup/config.toml
cargo run --release
```

The GitHub token comes from `$GITHUB_TOKEN`, or `gh auth token` when that is
unset. Tags and repository order live in
`$XDG_DATA_HOME/pr-roundup/tags.db`.

## Slack emoji

Each status is written into the copied text as configured under `[emoji]` in
the config file. Slack shortcodes such as `:git-merged:` become emoji when the
message is sent; the page itself always shows Unicode icons.
