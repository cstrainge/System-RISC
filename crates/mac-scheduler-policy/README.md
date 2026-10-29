# Priority-band policy

This allocation-free policy consumes genuine SCHD candidate snapshots. It gives
interactive wakeups band2, ordinary applications/System work band1 and module
workers band0. Each20ms continuously runnable adds one band, capped at3. Within
a band it chooses the oldest runnable interval, then last-hart locality and stable thread identity. Core must reset runnable-since
when a dispatched thread re-enters the ready set; unchanged waiting rows retain
that timestamp. None of these fields grants dispatch authority.

Preferred quanta are2ms for upper bands,4ms for band1 and8ms for band0, clamped to
actual core limits. These are policy choices, not measured response/starvation
bounds. No allocation, logging, locking, callbacks or host services occur in
selection. Core retains maintenance/pumps, validates live claims and performs
context switches. Multi-hart safety, runtime timing, affinity/migration and
fallback still require the real native consumer and evidence.
