# Beads File-Remote Archive Recovery

The selected Wrix Beads remote contained a 406,592,026-byte Dolt archive,
`bhuvrul70vqbhein91gp03jetlvig3tj.darc`, above GitHub's per-file limit. Normal
`wrix beads push` correctly refused publication before creating a sync commit.
This was a storage-layout problem, not a reason to rewrite issue history or
weaken the publication guard.

## Lossless, Bounded Compaction

An initial decomposition retained all 217,543 archive chunk identities and
passed Dolt clone/fsck. It recovered 21 chunks from a missing parent table and
retained the original compressed records. However, expanding into hundreds of
table files crossed Dolt's conjoin threshold: a later normal push generated
another 407 MB archive. That layout was replaced, not accepted as the final
repair.

The final offline compaction retained **all 218,044 chunk identities**
referenced by the newer remote manifest, including unreachable chunks. A GC
trial supplied most original Snappy records but omitted 276 unreachable chunks.
Those records were recovered directly from the original archive, including their
original CRC32C values; they were not discarded. Compressed records, chunk
hashes and uncompressed-size accounting were preserved in thirteen NBS tables,
each at most 33,554,371 bytes. A fresh Dolt clone and full `dolt fsck` passed,
including all 9,948 commit objects and the retained unreachable data.

Keeping the table count bounded avoids the immediate conjoin that defeated the
first decomposition. The GitHub size guard remains unchanged; future store
growth still requires ordinary storage maintenance rather than an exception.

## Concurrency and Recovery

Native synchronization, Git synchronization and the private remote's existing
writer lock were held in their normal order during cutover. An earlier attempt
correctly aborted when the remote advanced during offline preparation. The final
cutover preserved the latest root `dmj8g4u30a6fpm0os0b6dj5tmtd0m7a4` and its
newer delta tables, leaving fifteen referenced tables. A fresh storage CAS token
made readers refresh the changed table layout. Logical roots, SQL data, lock
inodes, running services, authorization and published Git history were
unchanged.

Old table representations were removed only after their complete chunk sets were
proven redundant with the retained compacted data. Copy-on-write recovery
snapshots and codec/proof records remain under the local
`.loom/recovery/wx-49uod-dolt-archive/` recovery area. Disposable packing trials
and verification clones are cleaned up; the retained snapshot is not a second
writable database.

Ordinary `wrix beads push` succeeded after cutover and again after a new Beads
metadata update. No force push, database-history deletion, GitHub-limit
exception or service upgrade was used.
