# Nisaba app persistence

The app service always uses durable adapters. Startup runs the embedded SQLx migrations from
`../../migrations` and requires PostgreSQL and S3 configuration; missing or unreachable
dependencies are fatal. In-memory repository and blob-store implementations are compiled only
for unit tests and cannot be selected by the service binary.

Production fulltext bytes are stored in the S3-compatible bucket configured by
`NISABA_S3_ENDPOINT`, `NISABA_S3_ACCESS_KEY`, `NISABA_S3_SECRET_KEY`, optional
`NISABA_S3_REGION`, and `NISABA_S3_BUCKET_BLOBS`. Objects are keyed as `fulltext/<reference UUID>`;
the key is the stable reference identity and never a citation number. PostgreSQL stores only the
fulltext metadata and blob reference. The exporter fetches bytes from the blob store when it
builds the reference export.

`TEST_DATABASE_URL` is reserved for optional PostgreSQL adapter integration tests. Without it,
unit tests remain pure in-memory tests and the migration file remains inspectable without a
running database.
