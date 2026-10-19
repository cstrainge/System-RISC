# Original Standard File Package evidence

System 7.6.1, private QEMU q800/Quadra guest. The Retro68 fixture calls
StandardGetFile with the TEXT type list, then StandardPutFile with a selected
`resources.json` default. Escape cancels each. Captures retain the actual
original UI; runtime.jsonl retains original StandardFileReply outcomes.
DLOG/DITL bytes are extracted directly from that guest's System resource fork.
`provenance.json` records binary/source/media/resource/capture hashes and the
Inside Macintosh API references. The capture script transports input and evidence;
canonical Rust Toolbox tests own acceptance. No generated boot disks are tracked.
