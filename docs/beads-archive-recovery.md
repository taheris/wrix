# Beads File-Remote Archive Recovery

The selected Wrix Beads remote contained a 406,592,026-byte Dolt archive,
`bhuvrul70vqbhein91gp03jetlvig3tj.darc`, above GitHub's per-file limit. Normal
`wrix beads push` correctly refused publication before creating a sync commit.
This was a storage-layout problem, not a reason to rewrite issue history or
weaken the publication guard.

## Lossless Decomposition

The archive's authenticated chunk index contained 217,543 chunk identities. Its
metadata identified the 257 source tables that had been conjoined. Of these, 256
remained available, containing 217,522 distinct chunks. The missing table's 21
chunks were recovered byte-for-byte from a separately cloned and verified Dolt
store. A new NBS table retained their original compressed records, lengths,
hashes and uncompressed-size accounting; its content-addressed name was derived
from its suffix index. The full resulting chunk identity set exactly matched the
original archive.

An offline copy replaced only the large archive's table-manifest entry with
those original tables and the recovered table. Its database root was unchanged.
A fresh Dolt clone and full `dolt fsck` succeeded, including all 9,934 commit
objects. Every resulting file was below GitHub's 100 MiB limit. An initial GC
trial was not used: GC still retained a large table, and pushing that trial
re-created a 313 MB archive.

## Concurrency and Recovery

Native synchronization, Git synchronization and the private remote's existing
writer lock were held in their normal order during final cutover. A first
cutover correctly aborted when the remote manifest had advanced since the
snapshot. The successful cutover retained the latest root
`luurif1nm5gu2b2550uqlb7fr79ofvvm`, all newer table entries, and the complete
original archive chunk set. It changed only table layout, not logical roots or
SQL data. Lock inodes, running services, authorization and published Git history
were not replaced or changed.

The original snapshot and manifests remain under the local recovery directory
`.loom/recovery/wx-49uod-dolt-archive/`; disposable packing trials and
verification clones were removed. This recovery snapshot is not a second
writable database.

Ordinary `wrix beads push` then succeeded, publishing Beads commit
`0f1453935fe1ae3a11cebea0eafb914a49d0b8b2`. No force push, history deletion,
GitHub-limit exception or service upgrade was used.
