# Project Self-Check Specification

PSC1. `.github/workflows/selfcheck.yml` MUST run on pushes to `Monoize-Claude` and on manual dispatch. It MUST check out the triggering commit.

PSC2. The workflow MUST run with read-only repository permissions. It MUST NOT publish packages, change production data, or deploy an application.

PSC3. The backend job MUST run locked Rust tests for all targets. The initial run MUST omit tests whose names contain `postgres_` and MUST NOT set a PostgreSQL test DSN.

PSC4. The backend job MUST run the PostgreSQL migration, dashboard, organization endpoint, request-log, organization aggregate, and Replica ingest regressions against six separate disposable databases. Production credentials MUST NOT be supplied to this job.

PSC5. PostgreSQL regressions MUST run after the ordinary Rust tests. A failed test MUST fail the job. Ignored capacity and image benchmarks are outside the default self-check.

PSC6. The frontend job MUST install locked dependencies, run frontend unit tests and type checks through the production build, and run npm launcher tests.

PSC7. The deployment job MUST run Python deployment and release-package tests and the Bash drain tests. Network routing tests MUST run in an isolated Linux network namespace.

PSC8. The documentation job MUST install locked dependencies and build all four documentation locales.

PSC9. Self-check results establish only the checks that completed successfully. They do not establish production deployment, browser acceptance, or benchmark capacity.

PSC10. The Apeiron job MUST build its frontend, test its Rust server, and run the Go tests in both existing Go module directories. It MUST NOT call a live video provider or billing endpoint.
