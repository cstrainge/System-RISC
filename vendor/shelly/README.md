# Shelly

Current version: **0.5.4**.

Shelly is a Unix-style shell written in Rust, with a typed scripting language for
commands, structured data, and network services. Pipelines carry typed values
between Shelly functions and ordinary byte streams between external programs.

<p align="center">
  <img src="./Shelly.png" alt="Shelly" width="50%">
</p>

Shelly runs on Linux, WSL, and macOS. Its core language features are implemented,
and the project is in stabilization before a potential rewrite and cleanup of
the prototype's internals.

## Install with Cargo

With a Rust toolchain supporting edition 2024 installed:

```sh
cargo install shelly-shell --locked
shelly
```

The Cargo package is named `shelly-shell`; the installed executable is `shelly`.
Make sure Cargo's binary directory, normally `~/.cargo/bin`, is on your `PATH`.

The executable includes its standard library, so no separate library installation
is needed. You can select a filesystem library through `$SHELLY_STD_PATH`;
`$SHELLY_STD_FOUND` reports the selected location. See the
[startup guide](https://github.com/cstrainge/shelly/wiki/Startup-and-Standard-Library)
for library selection and profile loading.

## Build from source

Use a Rust toolchain supporting edition 2024:

```sh
git clone --recurse-submodules https://github.com/cstrainge/shelly.git
cd shelly
cargo build --locked
cargo run --locked
```

This builds and launches the debug executable. You can also run it directly:

```sh
./target/debug/shelly
```

The `wiki/` submodule records the documentation revision for this checkout. If you
already cloned the repository, initialize it with:

```sh
git submodule update --init --recursive
```

When developing with Shelly as your login shell, validate release builds in a
separate directory so you do not overwrite the executable your sessions depend on:

```sh
cargo build --locked --release --target-dir target/test-validation
./target/test-validation/release/shelly
```

## Run your first script

Save this as `hello.shy`:

```shy
let $name = if $args.count > 0 { $args[0] } else { "world" }
echo "Hello, ${name}!"
```

Run it with the installed executable or your source build:

```sh
shelly hello.shy Mira
./target/debug/shelly hello.shy Mira
```

```text
Hello, Mira!
```

For a single command, use `-c`. For source supplied on stdin, use `-s`:

```sh
shelly -c 'echo "Hello from Shelly!"'
printf 'echo "Hello from stdin!"\n' | shelly -s
```

Running `shelly` in a terminal starts the interactive REPL. Ctrl+D on an empty
input exits; Ctrl+C interrupts a foreground command or discards the current input.
Use `shelly --help` for execution options.

## A brief look at the language

Commands use familiar space-separated arguments, while variables begin with `$`.
Functions can declare parameter and return types. Arrays, hashes, structs, enums,
and distinct named types let scripts describe the data they work with.

A pipeline can process structured values directly:

```shy
fn sum($args: Number...): Number
{
    $args.sum
}

let $result: [Number]
[[1, 1], [2, 2], [3, 3], [4, 4], [5, 5]] | sum $... | $result

echo $result...
```

```text
2 4 6 8 10
```

Each inner array becomes one pipeline item. `$...` spreads that item's values
into arguments to `sum`, and the final `$result` collects the returned sums.
External programs still connect through ordinary pipes, such as `git diff | less`.
Conversion annotations connect text formats such as JSON and CSV to typed stages.
[Visual pipeline splits](https://github.com/cstrainge/shelly/wiki/Pipeline-Splits)
let one upstream stream feed several independently transformed paths.

The language also includes:

- Integer and Float arithmetic, with Integer-to-Float promotion where required.
- Conditionals, match expressions, ranges, destructuring, and an iterator protocol.
- Closures with live captures, first-class function types, and extension methods.
- Modules with private declarations, public interfaces, and conditional imports.
- Typed errors and cleanup, plus library APIs for files, streams, JSON, and networking.
- An interactive editor with completion, syntax highlighting, history, and prompt widgets.

Shelly has its own language semantics; existing Bash scripts should continue to
run with their intended interpreter.

## Explore the wiki

The [Shelly wiki](https://github.com/cstrainge/shelly/wiki) contains the full
language documentation, worked examples, and standard-library guides. Start with:

- [Quick start](https://github.com/cstrainge/shelly/wiki/Quick-Start)
- [Language map](https://github.com/cstrainge/shelly/wiki/Language-Map)
- [Pipelines](https://github.com/cstrainge/shelly/wiki/Pipelines)
- [Standard library](https://github.com/cstrainge/shelly/wiki/Standard-Library)
- [Prompts and widgets](https://github.com/cstrainge/shelly/wiki/Prompts-and-Widgets)
- [VS Code support](https://github.com/cstrainge/shelly/wiki/Editor-Integration)
- [Vim support](https://github.com/cstrainge/shelly/wiki/Vim-Integration)

Shelly is available under the [MIT license](LICENSE).
