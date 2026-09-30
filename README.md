# unofficial-coder-sdk-rs

This is an unofficial Rust SDK for the Coder API, and it is not built, supported, or endorsed by Coder.

It exists to support scuttle, a personal terminal client for Coder Agents.

## Layout

- `crates/coder-api-gen` is generated from coder/coder's Swagger spec by progenitor and is never edited by hand.
- `crates/coder-sdk` is the hand-written layer: session discovery, errors, and the chat WebSocket streams.
- `xtask` runs the generator.
- `tools` holds the raw-field lister and the spec patch script.

## Regenerating

Run `scripts/regenerate.sh <coder-ref-or-path>` with a coder/coder tag, branch, commit, or local checkout path.
The script records what it applied in `spec/patches.log` and the source commit in `spec/coder-ref.txt`.

## License

The code in this repository is licensed under the MIT License; see `LICENSE`.
The API specification files under `spec/` come from coder/coder and remain under its AGPL-3.0 license; see `NOTICE`.
