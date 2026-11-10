# ls

A native CLAP directory-listing application for Terminal. It uses the public
Toolbox catalog and bounded directory scanner, including the calling process's
current directory and mount authority. It does not require Unix APIs or native
`std::fs::read_dir` support.

Long listings show three native flags: `d` for directory or `f` for file, `L`
when the catalog lock is set (`-` otherwise), and `A` for APPL/CLAP applications
(`-` otherwise). These are not Unix owner/group permissions, and an unlocked
entry does not guarantee that its volume is writable. `-F` marks directories
with `/` and APPL/CLAP applications with `*`. Application icons use those same
native file types rather than guessed filename suffixes or executable mode bits.

Directory scan failures, including stale enumeration after mutation, are
reported as errors. Pending bounded scan steps yield the application thread;
the next step uses the same native directory cursor. Returned catalog metadata
is reused for formatting, classification, and icons without another stat call.
