Original Finder 7.5.6 / System 7.6 Get Info resources.

The original fvew and STR resources are imported unchanged. Native gInf layouts
are semantic rectangle projections of the matching original fvew. Finder reads
this native resource descriptor through the public Resource Manager; it has no
HFS or resource-fork parser. The editable filename is a user-requested native
extension. TEXT documents use original fvew 17011, generic files 12761, and
folders 11261. Unknown file kinds retain the generic profile.

PROVENANCE.json retains source hashes, byte ranges, and rectangle projection
policy. Original runtime evidence is outside the OS repository in
results/finder-get-info-reference/RUNTIME-FINDINGS.md and its PNG/JSON captures.
Ordinary file/folder controls and geometry were checked against that runtime.
The APPL memory panel and application-specific stationery icon compositing are
outside this first UI profile; the generic stationery family is original System
icon resource -3985. Dates use checked Gregorian conversion with resource names.

Mounted volumes use original disk fvew11511 and its native rectangle projection.
Kind identifies the actual mounted filesystem and whole version; Size shows
capacity/free space without recursively scanning the disk. Volume dates come
from the header. Rename/comments remain disabled until the resident volume API
supports them. Pixel/runtime acceptance for this disk variant is separate from
the existing file/folder capture gate.
