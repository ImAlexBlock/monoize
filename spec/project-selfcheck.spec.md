# Project Self-Check Specification

PSC1. `.github/workflows/selfcheck.yml` MUST run on pushes to `Monoize-Claude` and on manual dispatch. It MUST check out the triggering commit.

PSC2. The workflow MUST run with read-only repository permissions. It MUST NOT publish packages, change production data, or deploy an application.

PSC3. The backend job MUST run locked Rust tests for all targets. The initial run MUST omit tests whose names contain `postgres_` and MUST NOT set a PostgreSQL test DSN.

PSC3a. A second library test run MUST include `postgres_` SQL-construction tests without a test DSN. It MUST omit the request-log test that requires that DSN. The ignored organization database test runs separately under PSC4.

PSC4. The backend job MUST run the PostgreSQL migration, dashboard, organization endpoint, request-log, organization aggregate, and Replica ingest regressions against six separate disposable databases. Production credentials MUST NOT be supplied to this job.

PSC5. PostgreSQL regressions MUST run after the ordinary Rust tests. A failed test MUST fail the job. Ignored capacity and image benchmarks are outside the default self-check.

PSC6. The frontend job MUST install locked dependencies, run frontend lint and unit tests, run browser fixtures with synthetic loopback data, run type checks through the production build, and run npm launcher tests. Its Node process MUST use a 4096 MiB maximum old-space heap because the default runner heap is insufficient for the current icon imports.

PSC7. The deployment job MUST run Python deployment and release-package tests and the Bash drain tests. Network routing tests MUST run in an isolated Linux network namespace.

PSC8. The documentation job MUST install locked dependencies and build all four documentation locales.

PSC9. Self-check results establish only the checks that completed successfully. They do not establish production deployment, browser acceptance, or benchmark capacity.

PSC10. The Apeiron job MUST build its frontend, test its Rust server, and run the Go worker tests in `apeiron/worker`. `apeiron/server` is a Rust crate and MUST NOT contain an incomplete duplicate Go module or worker tests. The job MUST NOT call a live video provider or billing endpoint.

PSC11. After all verification jobs succeed, the release job MUST build the Linux x86-64 release executable with the embedded frontend. It MUST upload the executable, SHA-256 checksum, source revision, and migration source tree as a private workflow artifact. Artifact creation MUST NOT deploy or publish a package.

PSC11a. Before uploading the release artifact, the release job MUST build its runtime image with the staged executable and repository Dockerfile. Run that image as UID/GID `1000:1000`, with no network, no published ports, and a temporary writable `/app/data`. Set the application listener to `127.0.0.1:8080` and use a new SQLite database. From inside the container, require `/readyz` to report ready, reachable SQLite, and the Primary role within 120 seconds. Require `/healthz` to return `ok`, `/` to serve HTML, and every referenced module script to serve nonempty JavaScript under `/assets/`. Send SIGTERM and require exit code `0` within 30 seconds. Any failure MUST prevent artifact upload. Retain a JSON result containing the source revision, executable checksum, migration-tree digest, image ID, readiness result, asset checksums, and exit code. Remove only the temporary test container and image. Do not collect runtime credentials or use production configuration.

PSC12. The rehearsal job MUST run the isolated rehearsal crate's tests serially against a disposable database named `lynshen_rehearsal`. It MUST use no production credentials. Passing these tests does not qualify maximum-envelope capacity or production data migration.
