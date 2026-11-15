# Contributing to FilePilot

Thanks for your interest. Bug reports, ideas and pull requests are welcome.

## Reporting a bug or asking for a feature

Open an [issue](https://github.com/movidiu64/FilePilot/issues). For a bug, please include:

- the FilePilot version (Settings, at the bottom) and your Windows version;
- the rule involved (rule menu → *Edit as JSON*), with personal paths replaced;
- what you expected and what happened, plus the journal entry or error message if there is one.

## Pull requests

For anything larger than a small fix, open an issue first so we can agree on the approach before you spend time on it.

- Code, comments, commit messages and documentation are in English. The interface is translated: add every new text to both `en` and `ro` in `crates/app/ui/i18n.js`.
- Add tests for new behavior. File operations must stay safe after a crash at any step; see the existing crash-recovery tests in `crates/actions/tests` and `crates/journal/tests`.
- `cargo test --workspace` must pass. CI runs it on Windows for every pull request, together with Clippy and an installer test.
- Describe user-visible changes under `## [Unreleased]` in `CHANGELOG.md`.

See [Development](README.md#development) and the [guide](docs/guide.md#architecture) for how the project is organized.

## License of contributions

FilePilot is licensed under the [GNU GPL v3](LICENSE) (GPL-3.0-only). So that the project can keep being maintained and, if needed, offered under other terms, contributions are accepted under these conditions:

By submitting a contribution (a pull request, patch or any code or text meant for inclusion), you confirm that:

1. you wrote it yourself, or otherwise have the right to submit it under these terms;
2. it is licensed under GPL-3.0-only, like the rest of FilePilot;
3. you also grant Ovidiu ([movidiu64](https://github.com/movidiu64)), the author and maintainer of FilePilot, a perpetual, worldwide, non-exclusive, royalty-free and irrevocable license to use, modify, sublicense and distribute your contribution under other licenses as well, including proprietary ones.

You keep the copyright in your contribution and may use it in any way you like. If you do not agree with point 3, please say so in the pull request instead of submitting it.
