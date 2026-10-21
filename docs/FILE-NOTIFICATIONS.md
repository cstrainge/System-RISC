# File and directory change notifications

Required for the filesystem-driver work and Finder cache refresh. This document
records the full target contract. Native directory subscriptions and open-folder
Finder refresh are implemented; file subscriptions, terminal notifications and
the Apple compatibility surfaces remain pending. See
[implemented contract and gates](FILESYSTEM-INTERFACE.md#directory-watches).

Resident File Manager owns subscriptions. Native Toolbox exposes an owned Rust
watch with `Drop`, backed by the shared generation-tagged object facility and
trusted application ownership. Bind to mounted-volume identity and persistent
CNID, not a pathname: rename/move must not silently retarget a subscription.
Directory watches report changes to their immediate contents; native file
watches report changes to that file. Moving a child changes both parent folders.
Unmount/deletion must have an explicit terminal notification and retire the
binding; remount or CNID reuse cannot revive it. App exit releases its watches.

Watch registration requires read access, **never write access**. Registration on
a read-only filesystem succeeds even if the provider cannot generate changes;
such a subscription is valid and quiet, not `ReadOnly` or `NotSupported`.
Validate object existence and ownership normally. Read-only host exports may
change outside the guest and should report supported external changes. An
immutable StuffIt mount can keep its content watches quiet. This does not weaken
any mutation permission checks.

Every successful native mutation publishes affected catalog revisions and
notifies the same resident machinery. External host changes enter that machinery
through bounded provider change reporting. Coalesce by subscription/object and
latest revision, with bounded storage; overflow produces an explicit rescan
signal rather than silently losing invalidation. Do no filesystem enumeration,
callback invocation or drawing while holding a notification-manager guard.
Deliver on the subscriber's event context, never inside the writer's syscall.
Unchanged input and pointer motion do no notification work. A quiet watch needs
no per-watch polling timer. Providers requiring external-change polling must
share bounded background work and must not enumerate on a UI click.

Finder subscribes while each folder window is open (including in background).
One coalesced change invalidates only that folder's cached snapshot, followed
by one consistent refresh with `ScanStale` handling. Retain notifications that
arrive during refresh so an older snapshot cannot clear a newer invalidation.
Repaint only changed item/window bounds; opening several windows on one folder
does not justify repeated provider scans for the same revision.

## Apple compatibility evidence

Apple's Universal Interfaces 3.4 `Files.h` documents `FNNotify`, `FNNotifyByPath`
and `FNNotifyAll` as directory broadcasts by application code. Its availability
annotations say OS X 10.0; CarbonLib and non-Carbon CFM are unavailable. It does
not declare `FNSubscribe`. The later Apple 10.4u `Files.h` declares:

- `FNSubscribe`, `FNSubscribeByPath`, `FNUnsubscribe`, and
  `FNGetDirectoryForSubscription`, available from OS X 10.1.
- `kFNDirectoryModifiedMessage = 1`.
- An implicit `FNNotifyAll` subscription, disabled by
  `kFNNoImplicitAllSubscription`.
- Foreground-coalesced delivery by default; `kFNNotifyInBackground` opts into
  background delivery.
- Subscription query returns the directory reference because paths may change.

Sources: [Apple Universal Interfaces 3.4 Files.h](https://github.com/elliotnunn/UniversalInterfaces/blob/master/3.4/Universal/Interfaces/CIncludes/Files.h),
[Apple 10.4u Files.h](https://github.com/phracker/MacOSX-SDKs/blob/master/MacOSX10.4u.sdk/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/CarbonCore.framework/Versions/A/Headers/Files.h),
and [Apple FNSubscribe documentation](https://developer.apple.com/documentation/coreservices/1565373-fnsubscribe).

Preserve these broadcast, subscription and delivery semantics when exposing
those entry points, backed by the resident watch core. Automatic native mutation
notifications and native file watches are System RISC additions. Do not label
the FN subscription API a System 7 trap or assign it an invented classic trap
number. Further original-system evidence is needed for any separate pre-Carbon
notification mechanism; the headers above do not establish one.

## Acceptance

Canonical assertions belong in `crates/mac-toolbox/tests` through public calls:
registration/drop, ownership, forced slot reuse, rename identity, deletion/eject,
coalescing and overflow, background delivery, wildcard opt-out and notifications
arriving during refresh. Cover native writes and externally modified host
exports, both old and new parent after a move, and quiet read-only subscriptions
without relaxing destination/source mutation rejection. Finder refreshes open
folders without manual refresh, unrelated cache invalidation or event storms.
