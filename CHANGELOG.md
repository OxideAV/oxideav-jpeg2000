# Changelog

All notable changes to `oxideav-jpeg2000` are recorded here.

## [Unreleased]

### Changed

* **Orphan rebuild (2026-05-20).** The crate was reset to a clean-room
  scaffold. The prior implementation contained module-level docstrings
  and inline comments whose provenance could not be defended against
  the workspace clean-room rule (no external library source as
  reference, not even as a sanity check). Per the workspace's
  Implementer-Round procedure, such audit failures are unrecoverable
  via incremental cleanup and require an orphan rebuild.

  Every public API path now returns `Error::NotImplemented`. A
  clean-room re-implementation against ITU-T T.800 / ISO/IEC 15444-1
  + ISO/IEC 15444-15 is planned for a future round.

  No `old` branch is retained; long-standing audit failures forfeit
  the archive per workspace policy.
