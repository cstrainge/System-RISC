# Shelly for System RISC

This source fork starts from [Shelly 0.5.5 at 4251473b](https://github.com/cstrainge/shelly/commit/4251473b23e5a3c94d7944129207e5ef34170e21).
The import includes the executable source, embedded standard library, runtime
assets, Cargo manifest and lockfile, and upstream MIT license.

The System RISC workspace packages Shelly as Terminal's default CLAP in
`Applications:Utility:Cli`. The port uses the platform launch and pipe
contracts, volume-qualified paths, and guarded mappings for same-thread
coroutine stacks. Guest builds omit networking and the Unix process
replacement and external pipeline implementations.
