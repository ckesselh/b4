# Security

`b4` reads files that hold people's banking data and, in parts it deliberately
does not touch, their online banking credentials. Reports of anything that
weakens the promises below are very welcome.

## What b4 promises

- **Bank access data is never decrypted**, in a vault as Banking4 wrote it
  (see the limit below). Vault entries that hold online banking credentials,
  keys, TANs or tokens are skipped by name, and their storage is reserved
  before anything else is decrypted. Access fields inside account and payment
  records are skipped while decoding: never turned into values, never stored,
  never written. Decrypted list contents are wiped from memory after decoding.
- **The vault is never modified.** It is opened read-only, read once and
  closed.
- **The password is never stored** and never accepted as a command-line
  argument.
- **`b4 sql` cannot leave the database.** Queries run read-only with DuckDB's
  file system access disabled and its configuration locked. A query can still
  use CPU and up to about 1 GiB of memory for as long as it runs; there is no
  time limit.
- **Hostile input does not crash or hang the reader.** No vault content makes
  `b4 decrypt` panic, and its work and memory stay bounded by the size of the
  file. Structural damage that b4 can detect is an error; damaged records are
  skipped with a warning. Because the format has no integrity protection,
  altered encrypted content can go unnoticed. The crate forbids `unsafe` code.
- **Text from a vault cannot drive your terminal.** Control characters and
  escape sequences are neutralised in everything written to a terminal.

What b4 does not promise:

- **The database is not encrypted.** It holds your bookings in plain form.
- **The vault's encryption is Banking4's**: AES in ECB mode with a
  password-derived key and no integrity protection. `b4` neither weakens nor
  strengthens it.
- **A vault that someone else has rewritten.** `b4` recognises bank access
  data by the names Banking4 gives its entries, and it refuses a vault in
  which a readable entry leads into the storage of an entry it does not read.
  But the format has no integrity protection, so nothing in a vault says whose
  bytes a piece of storage holds once its entry no longer carries its name.
  Someone who can modify your vault file could rename the entry with the bank
  access data to the name of an ordinary list; `b4 decrypt` would then decrypt
  those bytes and try to decode them as records. The reservation of unread
  storage is likewise a consistency check, not a barrier: it follows the
  directory as it finds it, so an entry that is marked deleted, or whose own
  directory flag or chain has been damaged, is not fully covered. The promise
  above holds for vaults as Banking4 wrote them. Decrypt only vaults that
  nobody else could have modified.

See the README, "What b4 reads and what it never reads".

## Reporting a vulnerability

Please do not open a public issue for a security problem. Use GitHub's private
reporting instead: on the repository page, **Security → Report a
vulnerability**. Say what you observed, how to reproduce it, and which version
(`b4 --version`) you used. Never attach a real vault, a real database or a
password; the synthetic demo vault in `tests/fixtures/vault` is usually enough
to show a problem.
