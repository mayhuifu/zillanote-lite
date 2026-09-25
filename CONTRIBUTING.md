# Contributing

Issues and pull requests are welcome. A few things make them easy to take in.

- **Say what you saw.** For a bug: the version (shown after the name in the window), the
  system, what you did, what happened, and the lines from `zillanote.log` (next to the
  meetings folder) around that time. Recordings and transcripts stay with you; describe
  them instead.
- **Build and test** as [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md) says: `cargo test`, `node --test ui/tests/*.mjs`, and
  the live tests when you touched what they cover. The Windows build job runs on a push to
  the `windows-port` branch of a fork with Actions enabled.
- **Keep the style of what is there:** comments that say why, names that say what, no
  new dependency without a reason in the pull request.
- **Third-party code and models** come with their license in `THIRD-PARTY-NOTICES.md`;
  add to it, and run `scripts/third-party-crates.py` after `Cargo.lock` changes.
- **License.** By contributing you agree that your contribution is licensed under the
  project's terms, MIT or Apache-2.0 at the user's option, with no other condition.
