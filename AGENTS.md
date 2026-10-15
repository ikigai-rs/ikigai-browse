# Working in this repository

`ikigai-browse` is an ikigai module: repository browsing as resources (tree, file and git state,
explanations, annotations, review findings, pull requests), with text, Turtle and HTML faces. These
rules are for every agent and every person working here. `CLAUDE.md` is a link to this file.

- **Read [ACCESSIBILITY.md](ACCESSIBILITY.md) before any change to the HTML faces or either
  stylesheet.** It says which parts of a page are this repository's and which are the host's, what
  is tested, and what is known to fail.
- Security reports go through the organization's security policy (GitHub private vulnerability
  reporting), never a public issue.
- Every change lands through a pull request with green CI: `cargo fmt`, `cargo clippy
  --all-targets -- -D warnings` and `cargo test`, with and without feature `migrate`, on Linux and
  macOS (`.github/workflows/ci.yml`).
- Never invent testing, tool results or conformance evidence, and never weaken a test to make a
  change pass.
- Use American spelling in everything you write: code comments, docs, commit messages and pull
  requests.
