# 1.0.0-beta Database Baseline

- `001_baseline.sql`: final schema, constraints, indexes, partitions and triggers.
- `002_data.sql`: initial sequence state; no sample users or runtime configuration secrets.
- `baseline.json`: baseline checksums and superseded pre-beta migration provenance.
- `000_drop_all_tables.sql`: manual development reset only; excluded from embedded migrations.

Rebuild the binary and execute `privchat migrate` against a **fresh database**.
The production runner owns locking, transactions, checksums and the migration
ledger. The shell helpers delegate to that runner. Do not apply these files
manually to an existing installation, erase only the ledger or bypass checksum
errors. Resetting a disposable test database is an explicit operator action,
not part of normal startup or this source change.

The previous pre-beta history has been intentionally rebased once. After beta
publication these two files are immutable; new changes start at `003`.
Application modules have their own schema/data pair and module migration ledger;
do not mix their SQL into this server's database migrations.
