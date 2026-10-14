![Lume](https://raw.githubusercontent.com/DeepBlueDynamics/lume/main/assets/lume-icon-96.svg)

# @deepbluedynamics/lume

**One store for the numbers and the words.** [Lume](https://github.com/DeepBlueDynamics/lume) keeps documents and time-series telemetry in one queryable system: hybrid search over the words, SQL over the measurements, and MCP tools for agents. One Rust binary, on hardware you control.

```sh
npm install -g @deepbluedynamics/lume
lume --version
```

Try it on a folder of Markdown, text, PDF or EPUB files:

```sh
lume index ~/Documents/manuals
lume search "bilge pump float switch"
lume sql repl
```

npm installs one prebuilt binary for your machine, from an optional dependency:

| Platform | Package |
|---|---|
| Linux x64 (glibc 2.35+) | `@deepbluedynamics/lume-linux-x64` |
| Linux arm64 (glibc 2.35+) | `@deepbluedynamics/lume-linux-arm64` |
| macOS Apple Silicon | `@deepbluedynamics/lume-darwin-arm64` |
| macOS Intel | `@deepbluedynamics/lume-darwin-x64` |
| Windows x64 | `@deepbluedynamics/lume-win32-x64` |

The binary is built with the `ti` feature, so `lume ti` (the telemetry index) is included. Don't install with `--omit=optional`: the binary is the optional dependency.

For the Signal K plugin, install `signalk-lume-ti` instead. For usage, see the [Lume README](https://github.com/DeepBlueDynamics/lume#readme).
