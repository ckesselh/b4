# 01 - The vault container

A Banking4 vault (`.sub`) is a single file holding a small file system: a fixed
header, an allocation table, directories and files. File contents can be
encrypted; everything else is stored in the clear. This document describes that
container in enough detail to write a read-only reader. What is stored *inside*
the files is the subject of the following documents: which entries a Banking4
vault holds is in [03-model-and-accounts.md](03-model-and-accounts.md), Part 1,
and how a file is cut into records in
[02-records-wire-format.md](02-records-wire-format.md). The
[README](README.md) has the overview.

The containers in `tests/fixtures/container/` were produced with Banking4,
and the manifest next to them records what a reader must return for them.
b4's reader follows sections 2 to 8 and is tested against that manifest.
Section 13 lists what is confirmed on those test files and what is not
confirmed; section 14 lists what is still unknown. All example data in this
document is synthetic.

## 1. Conventions

- Integers are little-endian. Unless stated otherwise they are signed 32-bit
  (`i32`); `u8` is one unsigned byte.
- Offsets are byte offsets, written in hex with a `0x` prefix.
- "Cluster" is the allocation unit. Clusters are numbered from 0.
- Field names are this document's own.

## 2. File layout

```
offset 0                      header region, one cluster_size long
                              (512 bytes of header, remainder zero)
offset 1 * cluster_size       cluster 0   (always the first allocation table page)
offset 2 * cluster_size       cluster 1
...
offset (n + 1) * cluster_size cluster n
```

- The header is always 512 bytes, but it occupies a whole cluster-sized slot at
  the start of the file. So **cluster `n` starts at file offset
  `(n + 1) * cluster_size`**. With 512-byte clusters the header slot is exactly
  the header; with larger clusters the bytes from 512 to `cluster_size` are
  unused (zero in files written by Banking4).
- The file length must be a multiple of `cluster_size` and at least
  `2 * cluster_size`. `cluster_count = file_length / cluster_size - 1`. Valid
  cluster numbers are `0 .. cluster_count - 1`.
- Three kinds of clusters exist: allocation table pages, directory pages and
  file data. Nothing in a cluster says which kind it is; that follows from how
  the cluster is reached.

## 3. Header

### 3.1 Fields

| Offset | Size | Type | Name | Meaning |
|-------:|-----:|------|------|---------|
| 0x000 | 4 | bytes | `magic` | `53 55 42 01` (`"SUB"`, then 0x01) |
| 0x004 | 4 | i32 | `cluster_size` | allocation unit in bytes, see 3.3 |
| 0x008 | 4 | i32 | `created_at` | creation time, see 3.4 |
| 0x00C | 4 | i32 | `root_dir_cluster` | first cluster of the root directory |
| 0x010 | 4 | i32 | `security_flags` | bit set, see 3.6 |
| 0x014 | 4 | i32 | `cipher_id` | cipher for file contents, see 3.7 |
| 0x018 | 4 | i32 | `kdf_rounds` | number of key-stretching rounds, see 5.2 |
| 0x01C | 32 | bytes | `salt` | random; input of key stretching and password check |
| 0x03C | 32 | bytes | `salt_check` | `salt` encrypted with the pass key, see 5.3 |
| 0x05C | 20 | bytes | `legacy_area` | not read or written by Banking4; zero in every file it wrote |
| 0x070 | 16 | GUID | `file_guid` | identity of this container file, see 3.5 |
| 0x080 | 16 | GUID | `type_guid` | application-defined content type, see 3.5 |
| 0x090 | 4 | i32 | `failed_attempts` | counter of wrong password attempts, see 3.8 |
| 0x094 | 32 | bytes | `token_salt` | all zero when no access token was issued, see 5.6 |
| 0x0B4 | 32 | bytes | `token_check` | `token_salt` encrypted with the content key, see 5.6 |
| 0x0D4 | 4 | i32 | `modified_at` | time of the last header write, see 3.4 |
| 0x0D8 | 40 | bytes | `reserved` | unused, zero |
| 0x100 | 256 | bytes | `wrapped_key` | content key encrypted with the pass key, see 5.4 |

Total 0x200 = 512 bytes.

### 3.2 Validation

Banking4 validates exactly one thing in the header: the four magic
bytes. If fewer than 512 bytes can be read, or the magic differs, the file is
"not a container".

There is **no header checksum**. Banking4 contains an unused digest:
RIPEMD-160 over the 16-byte constant
`80 d6 1d ca 1d ab 48 bd b8 6c f4 be 6f 75 00 8f` followed by a byte string
it is given. Nothing uses it: not reading, writing,
creating or copying a container. The 20-byte `legacy_area` has the size of a
RIPEMD-160 digest and may have held such a value in an earlier format revision
(see open questions). A reader must not expect or check anything there:
Banking4 opens a container whose `legacy_area` and `reserved` bytes are
filled with garbage without complaint.

### 3.3 Cluster size

The stored value is coerced: 1024, 2048, 4096, 8192, 16384, 32768 and 65536 are
taken as they are; **every other value, including 0 and nonsense, means 512**.
A reader should apply the same rule. Banking4's default for new containers
is 4096; when a container is created with an unsupported size it stores
512.

### 3.4 Timestamps

`created_at` and `modified_at` count whole seconds since
2000-01-01T00:00:00 UTC. Example: 820540800 (bytes `80 75 e8 30`) is
2026-01-01T00:00:00Z. The value 0 means "not set" (for `modified_at` Banking4
then reports the file system's modification time instead). Times
before 2000 are stored as 0. The i32 range ends on 2068-01-19.

`modified_at` is rewritten with the current time on every header write.

### 3.5 GUIDs

Both GUIDs use the Windows in-memory layout: the first three groups of the
textual form are little-endian, the last two are stored in reading order.
`01234567-89ab-cdef-0123-456789abcdef` is stored as
`67 45 23 01 ab 89 ef cd 01 23 45 67 89 ab cd ef`.

- `file_guid` is random, assigned at creation. If it is all zero, Banking4
  substitutes (in memory only) a GUID whose first four bytes are
  `created_at` and whose other bytes are zero.
- `type_guid` is chosen by the application that creates the container. The
  container layer never checks it. Banking4's configuration names
  `6ab4f284-82a0-46fd-83f4-438632912ba7` as the type of its data vaults (see
  open question 6).

### 3.6 Security flags

| Bit | Value | Name | Effect |
|----:|------:|------|--------|
| 0 | 1 | `device_bound` | a 16-byte device GUID is mixed into the pass hash (5.1) and into access-token keys (5.6) |
| 1 | 2 | (unknown) | none: Banking4 never tests it |

Only bit 0 of this bit set has a defined meaning. The flags value is
consulted in exactly one place in the container layer: the computation of the
pass hash (5.1) tests bit 0. That computation serves both the password key and
access-token keys, so bit 0 affects both and nothing else. No other bit is
ever tested. The stored value is available to the application, but Banking4
is not known to read it anywhere else. The Banking4 version described here (the
Windows build named in the [README](README.md)) creates its vaults with value 0
and, whenever the vault password is changed, stores a value taken from an
application configuration parameter. That parameter defaults to 0 and the
configuration of an installation of the Banking4 version described here does
not set it.

Value **2** (bit 1 set, bit 0 clear) has been reported for a real Banking4
vault. Its consequences: none. Key derivation, password check, content
decryption, allocation table and directories are all identical to value 0. This
is confirmed on test files produced with Banking4 (13.1, 13.3): v06 and v09
have value 2 and open with the pass hash and pass key of value 0, and v06,
which was created while a device GUID was supplied, opens without one. What
the creating application meant by bit 1 is unknown.

A reader should test bit 0 only and ignore all other bits. Value 3 behaves
like value 1.

### 3.7 Cipher

| `cipher_id` | Cipher | Content key length |
|------------:|--------|-------------------:|
| 0 | none | 0 |
| 1 | AES-128 | 16 bytes |
| 2 | AES-192 | 24 bytes |
| 3 | AES-256 | 32 bytes |

`cipher_id` selects the cipher for **file contents only**. The password layer
(5.2 to 5.4) always uses AES-256, whatever `cipher_id` says. Banking4 creates
its vaults with `cipher_id` 3.

Any other value behaves like 0: there is no content key. Banking4
cannot create a container with `cipher_id` 0 (it fails), and it crashes when
asked to read an encrypted file from a container whose `cipher_id` is not 1, 2
or 3. A reader should report "unsupported cipher" for anything but 1, 2, 3.

### 3.8 Failed-attempts counter

`failed_attempts` is a plain counter in the header. It is not touched when a
password is checked; the container layer only offers the application a way to
set it, which immediately rewrites the header (and thereby `modified_at`).
Banking4 is not known to use that. A reader reports the value at most and
never writes it.

## 4. Cryptographic primitives

- **AES** is standard AES (FIPS-197) with 128, 192 or 256-bit keys. This is
  confirmed on test files produced with Banking4: between them they use all
  three key sizes, and a stock AES implementation decrypts them (13.3).
- **RIPEMD-160** is the standard algorithm. This is confirmed on test files
  produced with Banking4: they open with pass hashes from an implementation
  that reproduces the eight published test vectors (13.3).
- **Mode: ECB.** Every use of AES in the container, without exception,
  processes independent 16-byte blocks: no IV, no chaining, no counter, no
  tweak derived from the cluster number, file or position. Encrypting the same
  16 bytes with the same key yields the same 16 bytes wherever they occur.
- **Padding: none.** Where the data is not a multiple of 16 bytes it is
  zero-filled to the next block boundary before encryption, and the true length
  comes from elsewhere (the directory entry or the key length).
- **Integrity: none.** There is no MAC, checksum or signature on the header,
  the tables or the data. Damaged ciphertext decrypts to garbage without any
  error from the container layer. Only the password check (5.3) detects a
  wrong key.

Any standard AES and RIPEMD-160 implementation can be used.

## 5. From password to content key

### 5.1 Pass hash (32 bytes)

1. Feed RIPEMD-160 with, in this order:
   1. the 32-byte constant
      `90 3c 4a 16 40 b4 43 81 b1 99 86 6a a4 51 6d 19 0c b0 dc b8 ef a1 4c 1a 97 ce 04 3b 34 1a 34 cf`;
   2. the password encoded as UTF-16LE: two bytes per UTF-16 code unit, low
      byte first, characters outside the BMP as surrogate pairs, no byte order
      mark, no terminator. The password is used exactly as given: no trimming,
      no case folding, no Unicode normalisation;
   3. only if `security_flags` bit 0 is set: the 16 bytes of the device GUID in
      the layout of 3.5 (16 zero bytes if the device GUID is the empty GUID).
2. Take the 20-byte digest `d` and expand it to 32 bytes with a running XOR
   that cycles through the digest:
   `h[0] = d[0]`, and `h[i] = h[i-1] XOR d[i mod 20]` for `i = 1 .. 31`.

The result `h` is the pass hash.

### 5.2 Key stretching (pass key, 32 bytes)

Start with `key = pass hash`. Repeat `kdf_rounds` times (not at all if
`kdf_rounds` is zero or negative):

1. Set up AES-256 with the current `key`.
2. Encrypt the 32-byte `salt` as two independent blocks (bytes 0..15 and
   16..31), giving 32 bytes `x`.
3. `key[i] = key[i] XOR x[i]` for all 32 bytes.

The key schedule is rebuilt in every round because the key changes. The final
`key` is the pass key. The default for new containers is 100000 rounds;
Banking4 creates its vaults with that value.

`kdf_rounds` is a signed value in a header that nothing protects (section 4),
and the stretching runs before the password can be checked. Banking4
runs whatever number it finds, up to 2^31 - 1 rounds. Two reader
requirements follow:

- **A stored value of zero or below means zero rounds.** Read the field as
  signed and never reinterpret it as unsigned: a negative value would turn
  into billions of rounds.
- **A reader must refuse more than 10,000,000 rounds** with an error of its
  own, before it starts stretching. This is a deliberate deviation from
  Banking4, which has no limit. The cap is one hundred times the value
  Banking4 writes; 100000 rounds take a few milliseconds and the cap takes
  under a second, while the largest stored value would mean more than two
  billion key schedules for a file that needs neither a valid password nor
  valid contents to demand them.

### 5.3 Password check

Decrypt `salt_check` with AES-256 under the pass key (two independent blocks).
The password is correct if and only if the result equals `salt`. Nothing else
is consulted, and nothing is written to the file whether the check succeeds or
fails.

### 5.4 Recovering the content key

Let `n` be the content key length for `cipher_id` (16, 24 or 32). Decrypt the
first `ceil(n / 16)` blocks of `wrapped_key` with AES-256 under the pass key
and keep the first `n` bytes. That is the content key used for all encrypted
files in the container.

The key is zero-filled to a block boundary before it is encrypted, so 16
bytes of `wrapped_key` are meaningful for AES-128 and 32 bytes for AES-192 and
AES-256. The rest of the 256-byte field is zero and is ignored on reading.

The content key is random and independent of the password. Changing the
password re-creates `salt`, `salt_check`, `kdf_rounds`, `security_flags` and
`wrapped_key`, but leaves the content key and all clusters as they are.

### 5.5 Empty password

An empty password is legal. The pass hash is then the hash of the constant
alone (plus device GUID if bit 0 is set):
`81 1e 64 d0 49 cd 23 5c 82 54 ee 2f 1e e1 19 67 0d aa 2e ac 2d b2 c8 7c e5 61 8f f0 2e f8 42 83`.
When a container is created with an empty password Banking4 stores
`kdf_rounds = 0`, so the pass key equals the pass hash. Opening is the normal
procedure with the empty string; a reader should use the `kdf_rounds` value
found in the header rather than assume 0. Trying the empty password first is a
cheap way to detect an unprotected container. (Banking4 distinguishes an
absent password, which it rejects as a mistake of the caller, from an empty
one.)

### 5.6 Device binding and access tokens

**Device binding.** With `security_flags` bit 0 set, the container can only be
opened by a caller that supplies the same device GUID that was used when the
password was set. The GUID is not stored in the container; Banking4 passes its
per-installation application GUID. Where that GUID is kept is not known. A
reader that meets bit 0 needs the GUID as extra input, and a wrong GUID is
indistinguishable from a wrong password.

**Access tokens.** Banking4 can issue a 32-byte token that opens the
container without the password (used for unattended access). Only two header
fields belong to this mechanism, `token_salt` and `token_check`; the token
itself is handed to the caller and is not stored in the container. Where
Banking4 keeps it is not known. In outline:

- a token key is derived like a pass key, with a caller-chosen usage string in
  place of the password, `token_salt` in place of `salt`, and exactly one
  stretching round;
- the token is the content key, zero-filled to 32 bytes, AES-256 encrypted
  under the token key;
- `token_check` is `token_salt` encrypted with the content cipher under the
  content key, and is what a presented token is verified against;
- an all-zero `token_salt` means no token has been issued. Revoking tokens
  zeroes both fields;
- issuing a token changes `token_salt`, `token_check` and `modified_at` and
  nothing else in the file.

b4 does not need tokens. A reader must never issue or revoke one, because both
operations rewrite the header.

## 6. Allocation table

The allocation table has one 4-byte entry per cluster and is itself stored in
clusters ("table pages"). It is **not encrypted**.

### 6.1 Entry encoding

| Byte | Meaning |
|-----:|---------|
| 0 | status: `0x00` free, `0x01` chained, `0xFF` end of chain |
| 1..3 | next cluster number, 24-bit little-endian; meaningful only when status is `0x01` |

- Free entries are written as `00 00 00 00`, end-of-chain entries as
  `FF 00 00 00`.
- `01 06 00 00` means "the chain continues in cluster 6".
- Any other status byte is corruption.

### 6.2 Where the table pages are

`entries_per_page = cluster_size / 4` (128 for 512-byte clusters, 1024 for
4096-byte clusters).

- Table page `k` is stored in cluster `k * entries_per_page` and holds the
  entries for clusters `k * entries_per_page .. (k + 1) * entries_per_page - 1`.
  The entry for cluster `c` is therefore in page `c / entries_per_page` at byte
  offset `(c mod entries_per_page) * 4`.
- Consequently the first entry of every table page describes the page itself.
  It links the table pages together: it is either end-of-chain (this is the
  last table page) or chained to exactly `own cluster + entries_per_page` (the
  next table page). Any other content is corruption.
- Cluster 0 is always table page 0.

To load the table: read cluster 0; while the page's own entry is not
end-of-chain, check that it points to `current + entries_per_page` and read
that cluster as the next page. Banking4 loads all table pages when it
selects a file, before any password is involved.

Entries for cluster numbers at or beyond `cluster_count` are ignored; they are
zero in files written by Banking4. Conversely, the loaded pages may
cover fewer clusters than the file has (if a page's own entry says
end-of-chain too early). A cluster whose entry would lie beyond the last
loaded table page has no entry, and any reference to it is corrupt.

### 6.3 Following a chain

A file or directory is a singly linked chain of clusters. It is identified by
its first cluster; the allocation table gives each cluster's successor.

For a cluster `c` that is part of a chain:

- `c` must satisfy `0 < c < cluster_count`. Cluster 0 is never part of a data
  or directory chain.
- `c` must not be a cluster that holds a loaded page of the allocation table
  (reader requirement, below).
- Look up the entry of `c`. End-of-chain: the chain ends with `c`. Chained to
  `next`: `next` must satisfy `0 < next < cluster_count`; continue there.
  Free, or chained to 0, or chained beyond the file, or no entry at all
  (6.2): the chain is corrupt.

Banking4 has no protection against chains that loop. It follows a
directory chain to its end, so a looping directory chain never terminates; a
file chain is only followed as far as the file's size requires, so a file
chain that loops within that length returns the same clusters again as if
they were further contents.

**Reader requirement: no table page in a chain.** Cluster 0 is not the only
cluster the table is stored in: every table page that was loaded (6.2) is
cluster `k * entries_per_page`, and its own entry, which links the pages,
looks like the end or the middle of an ordinary chain. A reader must refuse
every loaded table page as a member of a file or directory chain, as the
first cluster of an entry or as a successor, and fail with "chain corrupt".
This is a deliberate deviation from Banking4, which excludes
cluster 0 only and would return the table's bytes as file contents or as
directory entries. No writer puts data there. A cluster in such a position
beyond the last linked table page is not a table page; it has no entry at
all and is corrupt for that reason (6.2).

**Reader requirement: no cluster twice in one chain.** A reader must remember
the clusters it has taken from the chain it is following and fail with "chain
corrupt" as soon as the chain leads to one of them a second time. This is a
deliberate deviation from Banking4. Counting clusters and failing
once a chain is longer than `cluster_count` is not enough: a file whose only
cluster links to itself, with a `size` of two clusters, is read as `[c, c]`
long before that count is reached, and the repeated plaintext then decodes
as duplicate records. The test applies to the clusters a read steps into. A
file chain that loops only beyond what `size` requires is never followed that
far and reads normally, as in Banking4 (8.1).

**Reader requirement: a cluster belongs to one entry.** A chain is the
contents of exactly one file or directory. Nothing in the table says which,
so a damaged or crafted container can make two directory entries lead into
the same clusters: several files sharing data, a file whose chain runs
through directory pages, a directory that is its own ancestor. Such entries
are **cross-linked**. A reader must keep track of which entry each cluster
was read for (the root directory counts as one) and fail with a
corrupt-container error when a second entry, file or directory, reaches a
cluster that was already returned for another. Reading the same entry again
is no clash. This too is a deliberate deviation: Banking4 notices
nothing and returns the shared data once per entry. Without the rule the
same bookings can appear under several accounts, and N directories that each
list a file over the same N-cluster chain cost N² cluster reads. With it the
work a container can cause is bounded by its size, and a reader that walks
the tree needs no separate protection against directory loops (7.4).

**Reader requirement: failed reads are bounded too.** The rule above bounds
the reads that succeed, because only returned contents make a cluster belong
to an entry. A read that fails returns nothing, so many entries that lead
into one damaged chain would each walk it to the fault again. A reader must
therefore also remember the clusters a failed walk went through, with the
number of clusters the chain still supplies from each of them and the fault
that follows. A later walk that reaches such a cluster and needs more
clusters than are left fails there, in one step, with the fault the first
walk found; one that needs no more than are left reads normally, which keeps
the legal case legal (a file chain that is damaged only beyond what `size`
requires). A cluster that two failed walks went through is given up: every
later entry that reaches it fails at once, even one that would have needed
fewer clusters than the chain still supplies. That can only happen when a
cluster further on was taken by another entry between the two walks, so it
concerns containers in which entries were already found to share clusters.
With both parts a cluster is stepped on by failed walks at most twice. This
is again stricter than Banking4, which has none of it.

**Reader requirement: fence off what is not read.** A reader that reads only
some of a container's entries, because the others hold what it must not see,
cannot leave it at skipping those by name. A name and a first cluster are two
fields of one unprotected directory slot, and all files of a container are
encrypted with the same key (5.4, 8.2): a slot that keeps a name the reader
reads and is given the first cluster, size and flags of an entry it skips
makes the reader decrypt the skipped entry's contents under the allowed
name. The rule above does not help by itself, because a cluster belongs to
the entry it was first *read* for, and the skipped entry is never read.

Such a reader must therefore, before it reads or decrypts the contents of
any file, **reserve** the storage of every entry it is not going to read:

- For a file, follow its chain in the allocation table from `first_cluster`
  to the end and record every cluster as belonging to that entry. The whole
  chain, whatever `size` says: the size is as unprotected as the first
  cluster, and a chain is one entry's as a whole.
- For a directory, do the same for its pages, decode them (they are stored
  in the clear, 7) and reserve every entry they list in the same way, to any
  depth. Nothing but the allocation table and these pages is looked at: no
  file contents are read and nothing is decrypted.
- Reserving is best effort and never an error. A chain that is corrupt, or
  that leads into a cluster which already belongs to an entry, is reserved
  as far as it could be followed. A cluster that already belongs to an entry
  stops the walk, which also ends directories that lead back into
  themselves.
- Reading the contents of a reserved entry is refused.

Afterwards the rule above does the rest: an entry whose chain needs a
reserved cluster fails as cross-linked. For that to keep the skipped bytes
unread, a file read must check **every** cluster that `size` requires
against the record before it copies or decrypts the first one (8.1). A
directory whose pages are reserved is not listed either, so the files of a
skipped directory cannot be reached through a second directory entry with an
allowed name.

Each cluster gets its first owner once and each directory slot is decoded
once, so reserving costs at most one step per cluster and one per slot,
whatever the directories look like.

The limit of the rule is that it follows the directories as they are found.
A container is not authenticated: one from which the slot of an entry was
removed, or in which an entry was renamed, gives a reader nothing to tell
whose contents a cluster once held. The rule keeps a reader from reading an
entry it skips through another entry; it cannot say where ciphertext came
from.

### 6.4 Limits

- Cluster numbers in the table are 24 bits wide, so no chain can reference a
  cluster above 16,777,215. Banking4 declares a maximum of
  16,777,216 clusters but does not enforce it when reading.
- Banking4 handles file offsets and the file length as i32. A
  container of 2 GiB or more is outside what it can work with. A reader should
  compute offsets in 64 bits.

## 7. Directories

A directory is a chain of clusters; each cluster is one directory page.
Directory pages are **not encrypted**: names, sizes and flags of all files are
readable without the password.

The root directory starts at `root_dir_cluster` (1 in every container written
by Banking4, but a reader should use the header field).

### 7.1 Page and entry layout

A page holds `cluster_size / 64` entries of 64 bytes each (8 per 512-byte
cluster), with no page header.

| Offset | Size | Type | Name | Meaning |
|-------:|-----:|------|------|---------|
| 0 | 4 | i32 | `first_cluster` | first cluster of the entry's chain; **0 = slot is free**; -1 = entry exists but has no chain yet, see 7.5 |
| 4 | 4 | i32 | `size` | length of the file in bytes; 0 for directories |
| 8 | 1 | u8 | `flags` | bit 0 (1) = contents encrypted, bit 1 (2) = entry is a directory |
| 9 | 1 | u8 | `name_len` | length of the name in bytes, 0..54 |
| 10 | 54 | bytes | `name` | UTF-8, `name_len` bytes, rest zero; no terminator required |

### 7.2 Reading entries

For each of the slots of each page, in order:

- `first_cluster == 0`: free slot, skip. A deleted entry is overwritten with 64
  zero bytes, so deleted and never-used slots look the same.
- `size < 0` or `name_len > 54`: Banking4 treats such a slot exactly like a
  free slot. Skip it.
- Otherwise it is a live entry. Decode the name as UTF-8; invalid sequences
  become U+FFFD rather than an error.

The order of the slots is the order in which Banking4 lists a
directory. Slots are not compacted: live entries and free slots are
interleaved, and a later page can hold entries while an earlier one has free
slots. A new entry takes the first free slot of the directory, so the listing
order is not the order of creation. All pages of the chain must be scanned. An
empty directory is a single page of zero bytes.

Two details of the name:

- `name_len` 0 with a non-zero `first_cluster` is a live entry with an empty
  name. Banking4 lists it but does not accept an empty name in a request, so
  it cannot open the entry. It never writes such an entry. A reader may list
  it; it cannot be addressed by a path.
- How many U+FFFD characters an invalid sequence turns into is a property of
  the UTF-8 decoder, not of the format. Banking4 produces one U+FFFD per
  broken sequence, the same as the lossy decoders of Rust and Python: one
  for a three-byte character cut after its first byte, for a four-byte
  character cut after two bytes and for a lone `0xFF`, and two for a lone
  continuation byte followed by a two-byte character without its second
  byte (two broken sequences). This may differ between the platforms
  Banking4 runs on and is not confirmed for all of them (open question 12).
  A reader should therefore keep the stored bytes as the identity of a name
  and treat the decoded text as display form.

### 7.3 Names

- Banking4 cuts names longer than 54 bytes at byte 54, even in the middle of
  a multi-byte character. Such a name then ends in an incomplete sequence and
  decodes with a trailing U+FFFD.
- Banking4 looks a name up by comparing the *decoded text* of each
  live entry with the requested text, character for character: case-sensitive,
  no Unicode normalisation, no trimming. The first match in slot order wins.
  For names that are valid UTF-8 this is the same as comparing the stored
  bytes.
- The requested name is not cut before the comparison. A file that was written
  under a name longer than 54 bytes is therefore not found under that name,
  only under the cut one. A name that was cut inside a character is found
  under its listed form, the one that ends in U+FFFD, and not under any other
  spelling.
- Because of the previous point a directory can hold several live entries with
  the same stored name: writing twice to the same over-long name creates a
  second entry instead of replacing the first. Banking4 then only
  ever reaches the first of them.
- There are no `.` or `..` entries and no path separators inside names. A path
  such as `Dir/file` is resolved by looking up `Dir` in the root directory and
  `file` in the directory that entry points to; the container layer itself has
  no notion of paths, and `Dir/file` is not found as a name.

A reader should compare stored bytes with the UTF-8 form of the requested
name, take the first match in slot order, and list every live entry, including
duplicates.

### 7.4 Entry kinds

- **File** (flag bit 1 clear): `first_cluster` and `size` describe the
  contents, see section 8. Bit 0 says whether the contents are encrypted. The
  choice is made per file; a container can mix encrypted and plain files.
- **Directory** (flag bit 1 set): `first_cluster` is the first page of the
  subdirectory, `size` is 0. If `first_cluster` is -1 the directory has no
  pages (Banking4 then treats it as not openable). Banking4 only
  ever writes value 2 for directories; the directory's pages are plain even if
  bit 0 were set.

Bits of `flags` other than 0 and 1 have no meaning. Banking4 reports
them unchanged and otherwise ignores them: an entry with flags `0x05` or `0x81`
is read as an encrypted file, one with `0x04` as a plain file.

Nothing prevents a corrupt container from having a directory that points back
to one of its ancestors, or two entries that share a chain. The container
layer of Banking4 has no tree walk of its own and detects neither. A reader
must: both are cross-linked entries in the sense of 6.3 and fail as such.

### 7.5 Entries without a chain, and files being written

`first_cluster` is -1 only for a short time. When Banking4 creates a
file, it writes the new entry to its directory page at once, with
`first_cluster` -1 and `size` 0, and collects the data in a new chain that no
entry refers to yet. When the writer closes the file, the entry gets the first
cluster of that chain and the final size. So -1 marks a file whose first write
is still going on, or was cut short by a crash. Such an entry is a live entry
of an empty file.

Replacing an existing file works the same way: the entry keeps its old chain
and size, and the old contents stay readable, until the writer closes the
file. Then the entry is switched to the new chain and the old chain is
released. A reader therefore sees either the old or the new version of a
file, never a mixture. The encrypted flag is decided when the entry is created
and survives replacement: new contents written to a plain entry stay plain
even if encryption was requested for them.

A file that was written and closed with no data does have a chain. It consists
of one cluster filled with zero bytes; the entry has that cluster as
`first_cluster` and `size` 0. Every empty file in the fixtures looks like that.
Banking4 does not shorten such chains, and a reader must not expect
`first_cluster` -1 for empty files.

## 8. File contents

A file's bytes are stored in its cluster chain in order: the first
`cluster_size` bytes in the first cluster, and so on. The directory entry's
`size` is the only record of the exact length.

### 8.1 Reading

```
remaining = size
cluster   = first_cluster
while remaining > 0:
    if cluster is "end of chain" (-1): error, chain shorter than size
    raw     = the cluster_size bytes of this cluster
    cluster = successor of this cluster per section 6.3
    used    = min(remaining, cluster_size)
    if the entry is encrypted:
        decrypt the first ceil(used / 16) blocks of raw, each 16-byte block
        independently, with the content cipher and the content key
    output the first `used` bytes
    remaining -= used
```

Details that matter:

- A file of size 0 produces no bytes and its chain is not consulted at all.
  `first_cluster` is normally a real cluster that holds zero bytes, and -1
  while the file is first being written (7.5). Whatever it is, even a number
  beyond the end of the container, Banking4 returns an empty file
  without complaint.
- The successor is looked up for every cluster that is read, including the
  last one, so the last cluster's table entry must be end-of-chain or a valid
  link. A chain that is longer than `size` requires is not an error; the extra
  clusters are ignored.
- The successor lookup includes the reader requirements of 6.3: a cluster
  that holds an allocation table page, one this chain has already supplied,
  or one that was returned for another entry ends the read with an error,
  and so does a cluster that an earlier failed read went through when the
  chain cannot supply what this read needs from there.
- **Check the whole chain before using any of it.** A reader that keeps
  entries unread by reserving their storage (6.3) must not produce the
  contents cluster by cluster as in the loop above: a chain that reaches a
  reserved cluster only in its last link would by then have been decrypted
  up to there. It walks the clusters `size` requires first, checks each
  against the record of who owns what, and copies and decrypts only when
  the walk has come back whole.
- **Never allocate by `size`.** The field is an unprotected `i32` in a
  plaintext directory page, readable before any password, and `0x7FFFFFFF`
  is a live length. A reader must not reserve `size` bytes before the chain
  has been followed: it either produces the contents cluster by cluster as
  above, or first walks the chain and fails with "chain corrupt" when it
  supplies fewer than `size` bytes. Whatever `size` says, a chain can supply
  at most `cluster_count * cluster_size` bytes, which is less than the length
  of the container file. Compute `ceil(used / 16)` and all offsets in checked
  or 64-bit arithmetic. (Banking4 streams as well; this is stated
  because the obvious implementation allocates first.)

### 8.2 What exactly is encrypted

For a file with the encrypted flag:

- Every full cluster of the file is 32 or more AES blocks, each encrypted on
  its own with the content key. The same key is used for every encrypted file
  in the container.
- In the last cluster only the blocks that contain file bytes are ciphertext:
  `ceil(used / 16)` blocks. If `used` is not a multiple of 16, the final block
  was zero-filled before encryption. Decrypt the whole block and discard the
  bytes beyond `used`.
- The rest of the last cluster, after those blocks, is plain zero bytes on
  disk. They are not ciphertext, and a reader has no reason to decrypt them.
- There is no per-file key, IV or header inside the clusters.

Because of ECB, a reader can decrypt any cluster of a file without having read
the ones before it.

### 8.3 Unencrypted files

For a file without the encrypted flag the clusters hold the bytes as they are,
with the unused tail of the last cluster zero-filled. They can be read without
the password.

### 8.4 Deleted data

Deleting a file zeroes its directory slot and marks its clusters free. The data
clusters themselves are not wiped and the container file does not shrink.
Deleting a directory first deletes everything in it, so the pages of a deleted
directory end up with all slots zeroed before they are released; file data
clusters are never touched. A deletion changes the header, table pages and
directory pages and nothing else. A reader must never interpret clusters that
are not reachable from a live directory entry.

## 9. Opening a container, step by step

Banking4 works in two phases: selecting a file (no password, reads
the structure) and opening it (password). A reader follows the same order.

**Phase 1: select**

1. Open the file for reading only. Do not request write access.
2. Read 512 bytes at offset 0. Fewer than 512 bytes available, or `magic` not
   `53 55 42 01`: not a container.
3. Decode the header fields. Coerce `cluster_size` (3.3).
4. Check the file length: a multiple of `cluster_size` and at least
   `2 * cluster_size`, otherwise the container is invalid. Compute
   `cluster_count`.
5. Load the allocation table pages (6.2). A read past the end of the file is a
   truncation error; a wrong page link is a corrupt allocation table.
6. Load the root directory by following the chain from `root_dir_cluster`
   (6.3) and decoding each page (7.2). After this step the names and sizes of
   the root entries are known, still without a password.

**Phase 2: open**

7. If `cipher_id` is not 1, 2 or 3: unsupported cipher (the password could
   still be checked, but encrypted files cannot be read).
8. If `security_flags` bit 0 is set, a device GUID is required.
9. If `kdf_rounds` is above 10,000,000: refuse (5.2; Banking4 has
   no such limit). Otherwise compute the pass hash (5.1) and the pass key
   (5.2), with no rounds for a stored value of zero or below.
10. Check the password (5.3). On mismatch: wrong password. Stop.
11. Recover the content key (5.4).

**Phase 3: read**

12. Resolve a path through directory entries (7.3), loading subdirectories on
    demand.
13. Read file contents (8.1), decrypting when the entry's encrypted flag is
    set.

A reader that does not read every entry reserves the storage of those it
skips between steps 11 and 13: after the directories it uses are listed
(step 12) and before the contents of the first file are read (6.3, "fence
off what is not read"). It needs no password.

In steps 6, 12 and 13 every chain is followed under the reader requirements
of 6.3 (no allocation table page in a chain, no cluster twice in one chain,
no cluster shared between entries, failed reads remembered, the storage of
skipped entries reserved first), and no buffer is sized by a directory
entry's `size` (8.1).

Steps 1 to 6 and reading unencrypted files need no password. Steps 9 to 11
need only the header.

## 10. Error conditions

Conditions a reader should distinguish. The middle column says how Banking4
reacts, to show which conditions are fatal there.

| Condition | Banking4 | Suggested reader error |
|-----------|----------|------------------------|
| File does not exist | "does not exist" result | not found |
| File locked by another process (Windows sharing or lock violation) | "in use" result | in use |
| Fewer than 512 bytes, or wrong magic | "not a container" result | not a container |
| Length not a multiple of `cluster_size`, or shorter than two clusters | "corrupt" result at select | invalid layout |
| A needed cluster lies beyond the end of the file | "corrupt" result at select; exception when it happens during a file read | truncated |
| Table page link is neither end-of-chain nor `own + entries_per_page` | "corrupt" result at select | allocation table corrupt |
| Table entry with a status other than 0x00, 0x01, 0xFF | exception when the entry is used | allocation table corrupt |
| Chain runs into a free entry, cluster 0 or a cluster beyond the file | exception when followed | chain corrupt |
| Chain starts at or runs into a cluster that holds a loaded allocation table page other than cluster 0 | not detected: the table page is returned as contents | chain corrupt (deliberate deviation, 6.3) |
| An entry whose chain reaches a cluster that an earlier failed read went through, and that needs more clusters than the chain supplies from there, or any entry that reaches a cluster two failed reads went through | each entry fails or reads on its own | the fault of the earlier read, found in one step; chain corrupt for a cluster that was given up (deliberate deviation, 6.3) |
| Chain ends before `size` bytes were read, including `first_cluster` = -1 with `size` > 0 | exception during read | chain corrupt |
| Reference to a cluster that no loaded table page covers | unspecific exception | allocation table corrupt |
| `first_cluster` of an entry is negative (other than -1), or at or beyond `cluster_count`, with `size` > 0 | exception: an unspecific I/O error when negative, a truncation error when beyond the file | chain corrupt |
| Directory chain that loops | not detected (does not terminate) | chain corrupt |
| File chain that reaches a cluster a second time within the length `size` requires | not detected: the repeated clusters are returned as contents | chain corrupt (deliberate deviation, 6.3) |
| Two entries whose chains share a cluster, including a directory that contains one of its ancestors | not detected: the shared data is returned for each entry; a caller that walks a tree with such a directory does not terminate | cross-linked entries, container corrupt (deliberate deviation, 6.3) |
| An entry the reader reads whose chain reaches a cluster of an entry it skips and has reserved, including a directory whose pages are those of a skipped directory | not detected: the contents of the skipped entry are returned, decrypted, under the name of the other | cross-linked entries, container corrupt, with nothing of the entry read; say that the container is damaged or was manipulated (deliberate deviation, 6.3) |
| Reading an entry that was reserved as one that is not read | not applicable | reserved (a mistake of the caller, not of the container) |
| `kdf_rounds` above 10,000,000 | not detected: runs that many rounds before checking the password | too many key-stretch rounds (deliberate deviation, 5.2) |
| Password check fails | "wrong password" result | wrong password |
| Bit 0 of `security_flags` set and no or wrong device GUID | "wrong password" result | device GUID required / wrong password |
| `cipher_id` not 1, 2, 3 | opens, then crashes on the first encrypted file | unsupported cipher |
| Encrypted entry but no content key (container not opened) | not possible | locked |
| Other I/O failure | "I/O error" result | I/O error |

A container that lost its tail on a cluster boundary still passes the layout
check, because `cluster_count` is taken from the file length. If only free
clusters were lost, nothing is wrong with it and everything reads. Otherwise
the first chain that leads into the lost part fails as "chain corrupt", since
its link now points beyond the file. There is one exception: the cluster of
an empty file. It is marked as used (7.5), but a file of size 0 is read
without consulting its chain (8.1), so a container that lost such a cluster
still reads cleanly although a used cluster is gone.

A directory entry's `size` is not an error condition by itself, however
large: a reader never allocates by it (8.1), and a size the chain cannot
supply ends as "chain corrupt" in the row above.

Not detectable at this layer: wrong or damaged file *contents*. There is no
integrity check, so a flipped ciphertext byte silently garbles 16 bytes of
plaintext. The record layer above has to cope with that.

## 11. What a read-only reader must never do

Banking4's read-only mode opens the file with read access and shared
reading, and a read-only select, open and read of every file leaves the
container byte-for-byte unchanged (confirmed on test files produced with
Banking4, 13.1). A reader must keep it that way:

- Never open the file for writing.
- Never write the header. In particular do not maintain `failed_attempts`, do
  not update `modified_at`, do not replace an all-zero `file_guid`, do not
  issue or revoke access tokens, do not change the password.
- Never write allocation table pages or directory pages, and never "repair" a
  corrupt chain.
- Never change the file length.

Also note:

- A wrong password changes nothing in the file; the password check does not
  count attempts by itself (3.8).
- While Banking4 has a vault open for writing, the file on disk can be
  momentarily inconsistent: it writes new data clusters first, then
  the directory page, and flushes its cached table pages afterwards. On Windows
  it holds the file exclusively during that time. Under Wine that lock does not
  stop a native Linux process, so a reader should treat chain and table errors
  on a vault that is in use as "try again later", not as proof of damage.
- Between the steps of a write the file on disk reads without error. While a
  new file is being written, its entry is there with `first_cluster` -1. A
  file that is being replaced has its old contents until the writer closes
  it. For a time the table marks clusters as used that no entry refers to
  yet (the chain of the file still being written). Opening a container for
  writing changes nothing in the file by itself; the header, and with it
  `modified_at`, is rewritten only when the container is closed after
  changes. What the file looks like inside a single step, where a directory
  page is already on disk and the table pages are not, is not confirmed.

## 12. Worked example

A complete synthetic container of 4096 bytes: 512-byte clusters, AES-256,
password `b4-example`, two stretching rounds, no device binding. It is built
from the values printed in this section and is not one of the test files
produced with Banking4. SHA-256 of the whole file:
`8c35aa4a62355886fbd739c2338c3f0d2ece07e881a47c2ee9344632106f571a`.

### 12.1 Layout

| File offset | Cluster | Content |
|------------:|--------:|---------|
| 0x0000 | - | header |
| 0x0200 | 0 | allocation table page 0 |
| 0x0400 | 1 | root directory page |
| 0x0600 | 2 | `hello.txt`, 20 bytes, encrypted |
| 0x0800 | 3 | `plain.txt`, 5 bytes, not encrypted |
| 0x0A00 | 4 | directory `Dir`, one page |
| 0x0C00 | 5 | `Dir/two.bin`, bytes 0..511, encrypted |
| 0x0E00 | 6 | `Dir/two.bin`, bytes 512..519, encrypted |

File length 4096 = 8 * 512, so `cluster_count` = 7.

### 12.2 Header

```
0x000  53 55 42 01                 magic
0x004  00 02 00 00                 cluster_size      = 512
0x008  80 75 e8 30                 created_at        = 820540800 (2026-01-01T00:00:00Z)
0x00C  01 00 00 00                 root_dir_cluster  = 1
0x010  00 00 00 00                 security_flags    = 0
0x014  03 00 00 00                 cipher_id         = 3 (AES-256)
0x018  02 00 00 00                 kdf_rounds        = 2
0x01C  10 11 12 ... 2e 2f          salt              (32 bytes, 0x10 counting up)
0x03C  65 73 56 35 7a a3 33 06 22 b6 cb 47 01 a8 cc 10
       a7 1d 92 cd be 77 3d 0e d1 a8 4d 52 e5 ed b5 cb    salt_check
0x05C  (20 zero bytes)             legacy_area
0x070  67 45 23 01 ab 89 ef cd 01 23 45 67 89 ab cd ef    file_guid 01234567-89ab-cdef-0123-456789abcdef
0x080  11 11 11 11 22 22 33 33 44 44 55 55 55 55 55 55    type_guid 11111111-2222-3333-4444-555555555555
0x090  00 00 00 00                 failed_attempts   = 0
0x094  (64 zero bytes)             token_salt, token_check: no token issued
0x0D4  90 83 e8 30                 modified_at       = 820544400 (2026-01-01T01:00:00Z)
0x0D8  (40 zero bytes)             reserved
0x100  02 a5 79 19 71 48 02 c4 b9 b2 10 9b 65 a2 42 23
       17 5a b6 8f 4c aa e3 c1 e6 6f bb a1 cb e7 14 e1    wrapped_key, first 32 bytes
0x120  (224 zero bytes)            wrapped_key, unused remainder
```

### 12.3 Key derivation

```
password as UTF-16LE   62 00 34 00 2d 00 65 00 78 00 61 00 6d 00 70 00 6c 00 65 00
RIPEMD-160(constant || password)
                       1403fb19a8697009e1f2a5f2914e8f33455ab73e
pass hash              1417ecf55d34444dac5efb0998d6596a2f75c2fce8eb1009a1c8b8b150a207f5
key after round 1      a430ad0916dcf3486233eb04b3cfd682d468ef6c67e378a9f9bf01eae499e481
pass key (round 2)     f9e49e025de6233ce6a3b6035eab5d41e968829cd7bc5796bb2fcfeb11969e4e
```

The first bytes of the pass hash show the running XOR: `14`, then
`14 XOR 03 = 17`, then `17 XOR fb = ec`, then `ec XOR 19 = f5`. Byte 20 is
byte 19 XOR digest byte 0 again.

Password check: AES-256-decrypting `salt_check` with the pass key gives
`10 11 12 ... 2f`, equal to `salt`.

Content key: AES-256-decrypting the first 32 bytes of `wrapped_key` with the
pass key gives `a0 a1 a2 ... be bf` (32 bytes, 0xa0 counting up).

For comparison, the same password with `security_flags` = 1 and device GUID
`00112233-4455-6677-8899-aabbccddeeff` (hashed as
`33 22 11 00 55 44 77 66 88 99 aa bb cc dd ee ff`) gives:

```
pass hash              824fed25295a080cbc759b6565c56e0db849fc34b67bd9111d6e3c388841af51
pass key, 2 rounds     5f7d4a8c34693819d877d4a1a59466999a060da6f2f08603f12f15ae772bd33d
```

With `security_flags` = 2 the results are those of flags 0.

### 12.4 Allocation table (cluster 0, offset 0x200)

```
ff 00 00 00   cluster 0: this table page; no further table page
ff 00 00 00   cluster 1: root directory, single page
ff 00 00 00   cluster 2: hello.txt, single cluster
ff 00 00 00   cluster 3: plain.txt, single cluster
ff 00 00 00   cluster 4: directory Dir, single page
01 06 00 00   cluster 5: chained to cluster 6
ff 00 00 00   cluster 6: end of the chain of two.bin
00 00 00 00   ... all further entries free (they lie beyond cluster_count)
```

### 12.5 Directories

Root directory (cluster 1, offset 0x400), three live entries, five free slots:

```
0x400  02 00 00 00  14 00 00 00  01  09  68 65 6c 6c 6f 2e 74 78 74  00 ...
       first=2      size=20      enc len "hello.txt"
0x440  03 00 00 00  05 00 00 00  00  09  70 6c 61 69 6e 2e 74 78 74  00 ...
       first=3      size=5       -   len "plain.txt"
0x480  04 00 00 00  00 00 00 00  02  03  44 69 72  00 ...
       first=4      size=0       dir len "Dir"
0x4C0  (five slots of 64 zero bytes)
```

Directory `Dir` (cluster 4, offset 0xA00):

```
0xA00  05 00 00 00  08 02 00 00  01  07  74 77 6f 2e 62 69 6e  00 ...
       first=5      size=520     enc len "two.bin"
```

### 12.6 File contents

`hello.txt` is the 20 ASCII bytes `Hello, synthetic b4!`. Twenty bytes need two
AES blocks, so cluster 2 starts with 32 bytes of ciphertext followed by 480
zero bytes:

```
0x600  c4 42 97 01 b0 d5 57 67 3a 17 1f 83 08 7f 65 2c    block 0
0x610  8d 1f 05 8b c2 0b ee 8e 8c 35 cc a2 bc 6a d7 47    block 1
0x620  00 00 00 ...                                       not ciphertext
```

Decrypting both blocks with the content key gives
`48 65 6c 6c 6f 2c 20 73 79 6e 74 68 65 74 69 63 | 20 62 34 21 00 00 00 00 00 00 00 00 00 00 00 00`;
the first 20 bytes are the file.

`plain.txt` is stored as `70 6c 61 69 6e` at 0x800, followed by zeros.

`Dir/two.bin` has 520 bytes, byte `i` being `(7 * i + 3) mod 256`
(SHA-256 `fd9aea67e2518f689c37aa9bf3305face76a9803d62b813687c4504c1d0cb539`).
Cluster 5 is 32 ciphertext blocks, beginning
`4a 8d c2 46 e8 44 69 4c cf 15 55 b9 31 a2 34 25`. The table entry of cluster 5
leads to cluster 6, where 8 bytes remain: one ciphertext block
`c8 44 5e 86 0e 7a a1 ac 02 60 1c 4f 20 0d fb c7`, then zeros. Its table entry
is end-of-chain.

## 13. Verification

13.1 describes the test files in `tests/fixtures/container/` and what they
confirm. 13.2 lists what Banking4 does with damaged containers. 13.3 says
what is confirmed about the constants, the primitives and the key
derivation, and covers the worked example. 13.4 lists what is not confirmed.

### 13.1 Fixtures produced with Banking4

The nine synthetic containers in `tests/fixtures/container/` were produced
with Banking4. `MANIFEST.json` next to them was produced together with them
and records, for each container, the header values, every entry in listing
order, and a digest of the bytes of every file.

| Fixture | Cluster size | Content cipher | Rounds | Flags | Special |
|---------|-------------:|----------------|-------:|------:|---------|
| v01 | 512 | AES-256 | 100000 | 0 | full content set; 2 table pages; root directory of 5 pages |
| v02 | 512 | AES-256 | 0 | 0 | same content as v01 |
| v03 | 512 | AES-256 | 0 | 0 | empty password; 100000 rounds were requested |
| v04 | 4096 | AES-128 | 1000 | 0 | a directory of 68 entries (2 pages of 64) |
| v05 | 1024 | AES-192 | 3 | 0 | password with umlauts, a euro sign, a Greek letter and an emoji; 2 table pages |
| v06 | 512 | AES-256 | 100000 | 2 | content type GUID of a Banking4 data vault |
| v07 | 512 | AES-256 | 10 | 1 | bound to a device GUID |
| v08 | 512 | AES-256 | 10 | 0 | one access token issued |
| v09 | 512 | AES-256 | 50000 | 2 | created with another password, 100000 rounds and flags 0; password changed afterwards |

Together they hold 464 entries: 410 files (146 of them empty) and 54
directories. The content is described in the README next to the fixtures.

A reader that follows sections 2 to 8 of this document arrives, for every
fixture, at what the manifest records. b4's tests check its reader against
the manifest. That covers:

- every header value in the manifest: cluster size, both timestamps,
  flags, cipher, rounds, both GUIDs in text form, the failed-attempts counter;
- the content key, for AES-128, AES-192 and AES-256;
- the whole tree in listing order: path, stored name bytes,
  flags and size of every entry;
- the contents of every file, by SHA-256 and byte for byte against the
  formula in the README next to the fixtures.

Beyond the manifest, the bytes of the fixtures confirm the following
statements of this document.

| Section | Confirmed |
|---------|-----------|
| 2 | Cluster `n` at `(n + 1) * cluster_size`; a 512-byte header in a slot of one cluster, the rest of the slot zero (v04, v05); `cluster_count` from the file length. |
| 3.1 | Field offsets; `legacy_area` and `reserved` zero; `root_dir_cluster` 1; `token_salt` and `token_check` zero unless a token was issued. |
| 3.4 | Seconds since 2000-01-01 in UTC: the fixtures were written at a local time two hours ahead of UTC, and both timestamps agree with the file's modification time taken as UTC. |
| 3.5 | GUID byte layout, for `file_guid`, `type_guid` and the device GUID (v07 opens only when the GUID is hashed in this layout, not in textual byte order). |
| 3.6 | Bit 0 (v07): opens with the right device GUID only; a missing, empty or different GUID is rejected. Value 2 (v06, v09): no device GUID is involved. v06 was created while one was supplied and opens without one. |
| 5.1 to 5.3 | Pass hash and stretching with 0, 3, 10, 1000, 50000 and 100000 rounds; UTF-16LE with a surrogate pair (v05). Rejected on every fixture: the password with a character appended, in another case, with a leading space, the empty password (a non-empty one for v03), and for v09 the password it was created with. |
| 5.4 | 16 meaningful bytes of `wrapped_key` for AES-128, 32 for AES-192 and AES-256, the remainder zero. A password change keeps the content key (v09, whose contents were written before the change). |
| 5.5 | Empty password: 0 rounds stored although 100000 were requested; the pass hash constant given there. |
| 5.6 | v08: the token in the manifest and `token_check` follow from the outline, with one stretching round and the usage string in place of the password. |
| 6 | Entry encoding; free entries `00 00 00 00`, end entries `FF 00 00 00`; table page positions and links; number of table pages = `ceil(cluster_count / entries_per_page)`; zero entries beyond `cluster_count`; chains that cross from one table page into the next (v01, v02, v05); chains that are not contiguous, from files written into freed clusters. Every cluster the table marks as used belongs to exactly one table page, directory or file: a finished container holds nothing else. |
| 7.1, 7.2 | Slot layout; 8, 16 and 64 slots per page; directories of up to 5 pages; live entries after free slots; free slots are 64 zero bytes; zero padding after the name; only flag values 0, 1 and 2; listing order = slot order; a new entry goes into the first free slot. |
| 7.3 | Names of exactly 54 bytes (ASCII and two-byte characters); names cut at byte 54, also inside a three-byte and a four-byte character; non-ASCII names. |
| 7.4, 7.5 | Directories have size 0 and flags 2; an empty directory is one zero page. All 146 empty files have a real first cluster that is zero-filled and marked end-of-chain, also when it was the last cluster written or a reused one; no finished container has a `first_cluster` of -1. |
| 8.1 to 8.3 | Sizes 0, 1, 15, 16, 17, 31, 32, 33, one cluster less, exactly and plus one byte, two clusters, up to 260 clusters; every block decrypted on its own; `ceil(used / 16)` ciphertext blocks in the last cluster, the final block zero-filled before encryption; the remainder of the last cluster zero on disk for encrypted and plain files, also in reused clusters. |
| 8.4 | Deleted files leave zeroed slots and free table entries; 8 of the 10 free clusters in v01 and v02 still hold old data, the other two are zero, as expected for the two pages of the deleted directory tree; free clusters at the end stay part of the file. |
| 11 | Reading in Banking4's read-only mode, including a rejected password, leaves a fixture byte for byte as it was. |

### 13.2 Behaviour on damaged containers

This is what Banking4 does with a container that is damaged, or that holds
values it never writes. The sections named in brackets say what a reader
should do, and section 10 shows where a reader's result differs from
Banking4's. b4's tests hold such cases with their expected results
(`tests/vault.rs`).

- **No header check beyond the magic (3.2).** Garbage in `legacy_area`, in
  `reserved`, in the unused part of `wrapped_key` and, in a container with
  4096-byte clusters, in the header slot beyond byte 512 changes nothing: the
  contents read as before. A changed first or fourth magic byte, and a file
  of 511 bytes, are "not a container".
- **Cluster size coercion (3.3).** 0, 256, 777 and -1 in a 512-byte container
  all read as 512.
- **Header fields without effect.** `failed_attempts` 7; a negative round
  count in a container with 0 rounds (treated as 0); flags 2 and
  `0x7FFFFFFE`; an all-zero `file_guid`; both timestamps zero. With each of
  them the container opens and reads as before.
- **Password layer.** Flags bit 0 set without a device GUID, one flipped bit in
  `salt` or in `salt_check`, and a changed round count: Banking4 reports
  "wrong password" in every case.
- **Cipher (3.7).** With `cipher_id` 0 or 4 Banking4 opens the
  container and fails on the first encrypted file.
- **Layout (2, 6.2).** A length one byte off, a file that is only a header, a
  wrong or free table page link: the container is invalid. A table whose
  first page claims to be the last one: the read fails on the first cluster
  without an entry.
- **Directory entries (7).** `name_len` 55 or 255 and `size` -1: the slot is
  skipped. `name_len` 0: the entry is listed; Banking4 cannot open it.
  Invalid UTF-8 in a name: replacement characters as described in 7.2. An
  embedded zero byte, and a `name_len` shorter or longer than the text: taken
  as they are. Flags `0x05`, `0x81` and `0x04`: bits above 1 ignored. A
  cleared encrypted flag: the ciphertext is returned as it is.
  `first_cluster` 0: the entry is gone. `size` smaller than the chain: the
  first bytes are returned; larger: the read fails (chain corrupt, 10).
  `first_cluster` -1, -2 or beyond the file with a size above 0: failure.
  `first_cluster` -1 or beyond the file with size 0: an empty file. A
  directory with `first_cluster` -1 is listed and has no contents; one with
  flags 3 or a non-zero size is read as a directory.
- **Chains (6.3, 8.1).** The entry of a file's last cluster set to free, to
  status 2, to a link to cluster 0 or to a link beyond the file: failure, so
  the successor is indeed looked up for the last cluster too. An end-of-chain
  entry with garbage in its link bytes, a chain that continues beyond what
  `size` needs, and a file chain that loops after the size is satisfied: read
  normally. A directory chain that loops: Banking4 does not terminate; a
  reader reports it (6.3).
- **Truncation (10).** A missing free cluster at the end: everything reads.
  Missing used clusters: the chains that lead into them fail (chain corrupt).
- **No integrity (4).** One flipped ciphertext bit: the damaged plaintext is
  returned without an error.

Three more statements that are not about damage:

- **Name lookup (7.3).** Banking4 finds a name as written, and does not find
  it in another case, with a trailing space or in decomposed Unicode form. An
  over-long name is found under its cut form only; a name cut inside a
  character under its listed form with U+FFFD only. Writing again under the
  same over-long name leaves two entries with identical stored names, and the
  lookup returns the first. A plain file rewritten with encryption requested
  stays plain.
- **Writes in progress (7.5, 11).** Between the steps of a write, while one
  new file is written and another is replaced, the container reads without
  error. Section 11 says what it shows at those points.
- **A missing file** gives the "does not exist" result.

### 13.3 Algorithms and the worked example

1. **Constants.** The 32-byte constant of 5.1 enters every pass hash, so it
   is confirmed on test files produced with Banking4: every fixture that
   opens depends on it (13.1). The 16-byte constant of 3.2 is not confirmed,
   because nothing uses it.
2. **Primitives.** Confirmed on test files produced with Banking4. The
   fixtures use AES-128, AES-192 and AES-256 between them and decrypt with a
   stock AES implementation, every block on its own. They open with pass
   hashes from a RIPEMD-160 implementation that reproduces the eight
   published test vectors.
3. **Key derivation.** Confirmed on test files produced with Banking4
   (13.1): the empty password, an ASCII password and one with umlauts, a euro
   sign, a Greek letter and an emoji; flags values 0, 1 and 2; 0, 3, 10,
   1000, 50000 and 100000 rounds; the byte order of the device GUID (v07).
4. **The worked example (12).** b4's tests compute the values of 12.3 and
   rebuild the example from the values printed in section 12. The result has
   the SHA-256 given there and the ciphertext printed in 12.6. It reads as
   described, with its four entries, and a wrong password is rejected for it.

### 13.4 Not confirmed

- **More than one vault created by Banking4 in normal use.** All fixtures
  are synthetic test files produced with Banking4, and so is the demo vault in
  `tests/fixtures/vault/`, which has the header values reported for a real
  vault (512-byte clusters, flags value 2), like v06 and v09. One real vault
  with those values has been read with b4's reader: all 92 files that were
  compared with data exported from Banking4 came out byte for byte the same.
  That is one vault of one program version; no other was read.
- **The reader requirements that go beyond Banking4** (the round limit of
  5.2, the four chain rules of 6.3, the allocation rule of 8.1) have no
  counterpart in Banking4. They are a matter of the Rust reader's own tests.
  None of them rejects a test file produced with Banking4.
- **Device binding in practice.** v07 uses a GUID made up for the fixture.
  Where Banking4 takes its device GUID from is unknown (14.7), so a real
  device-bound vault was not opened.
- **Access tokens beyond their value.** The token of v08 and its
  `token_check` follow from the outline in 5.6. Opening a container with a
  token and revoking tokens are not covered by the test files, and b4 does
  neither.
- **Cluster sizes** other than 512, 1024 and 4096 are not among the test
  files. Nor is a 4096-byte container with more than one table page. The
  container fixtures have at most two table pages; the demo vault in
  `tests/fixtures/vault/` has five.
- **`cipher_id` 0** as written by anything. No such container is known
  (14.5).
- **Limits.** Containers of 2 GiB or more, cluster numbers near 2^24,
  timestamps before 2000 or after 2068.
- **Platform differences.** File sharing and locking on Windows and the "in
  use" and "I/O error" results are not confirmed. How broken names are
  decoded may differ between the platforms Banking4 runs on and is not
  confirmed for all of them (7.2).
- **In-memory fallbacks.** Banking4 opens a container with an all-zero
  `file_guid` or a zero `modified_at` (13.2). The values it substitutes (3.4,
  3.5) are not confirmed.
- **Concurrent access.** Section 11 describes a container between the steps
  of a write. The state inside one step and what a reader sees of a vault
  that Banking4 has open in normal use are not confirmed. Neither is what
  the write buffering of Wine adds when Banking4 runs under it.
- **What Banking4 returns for a file chain that loops within the length the
  size needs, and for two entries that share clusters.** It has no test for
  either (6.3), but that is not confirmed. Nor is what it does with a
  directory that contains one of its ancestors (the container layer has no
  tree walk of its own; a caller that walks such a tree does not terminate).
  A reader refuses all three.
- **`failed_attempts`** as maintained by an application.

Of the statements made in sections 2 to 11, these are not confirmed: the values
substituted for an all-zero `file_guid` and a zero `modified_at`, the "in use"
and "I/O error" results, the i32 limits of 6.4, and the order of the writes
inside one step described in section 11.

## 14. Open questions

1. **Meaning of `security_flags` bit 1 (value 2).** A real vault is reported
   to have it set. Banking4's container layer never tests it and Banking4 is
   not known to read the flags anywhere else, so for reading it has no effect;
   what the application that stored it intended is unknown. The configuration
   of an installation of the Banking4 version described here, a Windows build,
   does not set the flag at all, which suggests that the vault was created, or
   had its password changed, by a different build or platform.
2. **Other creators.** The Banking4 version described here, a Windows build,
   creates data vaults with 4096-byte clusters and flags 0, while the one real
   vault that is known has 512-byte clusters and flags 2. Whether other builds
   (older versions, macOS, mobile) differ in anything beyond these header
   values is unknown. The format described here covers both cluster sizes, and
   that one vault reads correctly with it (13.4). Vaults of other builds were
   not read.
3. **`legacy_area` and the unused header digest.** What earlier format revision
   stored in the 20 bytes at 0x05C, and what the unused RIPEMD-160 digest with
   its 16-byte constant was applied to, is not known. Whether real vaults
   have non-zero bytes there is not known either. Nothing reads them either
   way.
4. **Fourth magic byte.** The 0x01 may be a format version. Banking4
   compares all four bytes as one value, and no other value is known.
5. **`cipher_id` 0.** Banking4 has a "no cipher" value but cannot create
   such a container and cannot read encrypted entries from one. Whether such
   files exist anywhere, and what an encrypted entry would mean in them, is
   unknown; b4 should report them as unsupported.
6. **`type_guid` of a real vault.** The GUID in 3.5 is taken from the
   application's configuration. The one real vault that is known is reported
   to carry it (`tests/fixtures/vault/README.md`); no other was looked at.
   Whether b4 should warn about or reject other type GUIDs is a design
   decision for later.
7. **Device GUID source.** For containers with bit 0 set, where Banking4 keeps
   its per-installation GUID is not known. Not needed as long as bit 0 is
   clear.
8. **Where access tokens are kept** by the application is not known. Not
   needed for password-based reading.
9. **`failed_attempts`.** Banking4 is not known to read or write the counter,
   so who maintains it (if anyone) is unknown.
10. **Reading a vault while Banking4 is running.** Between the steps of a
    write the file is consistent (section 11), but the order of the writes
    inside one step is not confirmed. Neither is what a reader sees of a
    vault that Banking4 has open in normal use, or what the write buffering
    of Wine adds to it when Banking4 runs under Wine.
11. **Containers of 2 GiB or more, or with more than 2^24 clusters.** Outside
    what Banking4 can handle; behaviour undefined.
12. **Broken names across platforms.** How many U+FFFD characters Banking4
    produces for a name that was cut inside a character may differ between
    the platforms it runs on and is not confirmed for all of them (7.2). It
    matters only for display and for looking such an entry up by its text;
    no name used by Banking4 is known to be that long.
13. **Real vaults.** One vault written by the Banking4 application has been
    read with b4's reader and compared with data exported from Banking4
    (13.4): no differences were found. Whether vaults of other program
    versions or platforms contain anything the synthetic fixtures do not
    show can only be settled by reading them the same way.
