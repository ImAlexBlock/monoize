# Dependency review: 2026-10-02

Audit with cargo-audit 0.22.2 and RustSec database revision
`db663534ae858abb3fbad408a041ce04209c377f`.
Use Cargo 1.99.0 for dependency resolution. Store the portable toolchain and caches
under `local-test/`. Do not infer runtime compatibility from dependency resolution.

## Applied changes

| Package | Previous version | Current version or disposition | Evidence |
| --- | --- | --- | --- |
| bytes | 1.11.0 | 1.12.1 | Resolves RUSTSEC-2026-0007. Raise the direct minimum to 1.11.1. |
| crossbeam-epoch | 0.9.18 | 0.9.21 | Resolves RUSTSEC-2026-0204. |
| h2 | 0.4.13 | 0.4.19 | Resolves RUSTSEC-2026-0258. |
| time | 0.3.44 | 0.3.55 | Resolves RUSTSEC-2026-0009. |
| pdf-extract / lopdf | 0.10.0 / 0.38.0 | Removed | No implementation, test, or build-script references use pdf-extract. Remove the unused direct dependency with Cargo. This removes RUSTSEC-2026-0187 without changing a document-processing flow. |
| rust_decimal / rkyv | 1.40.0 / 0.7.46 | 1.43.0 / removed | The old rkyv dependency was optional and absent from the enabled host dependency tree. Compatible rust_decimal updating removes it from the lockfile, resolving RUSTSEC-2026-0235. |
| anyhow | 1.0.100 | 1.0.104 | Resolves the RUSTSEC-2026-0190 unsoundness warning. |
| event-listener | 5.4.1 | 5.4.2 | Resolves the RUSTSEC-2026-0221 unsoundness warning. |
| rand | 0.8.5 / 0.9.2 | 0.8.8 / 0.9.5 | Resolves the RUSTSEC-2026-0097 unsoundness warnings within each compatible series. |
| chacha20 | 0.10.1 | 0.10.2 | Replaces a yanked version with a compatible release. |
| spin | 0.9.8 | 0.9.9 | Replaces a yanked version with a compatible release. |
| rehearsal rust_decimal | 1.42.1 | 1.43.0 | Removes the optional rkyv 0.7 lockfile dependency. |
| rehearsal rustls | 0.23.43 | 0.23.45 | Resolves the rehearsal TLS advisory. |

Run `cargo remove pdf-extract`, targeted `cargo update`, and `cargo add` for direct
minimum versions. Do not edit either Cargo lockfile manually.
The last targeted update changes only the versions and checksums of chacha20 and spin.

The main lockfile vulnerability count decreases from seven to one.
The rehearsal lockfile vulnerability count decreases from three to one.
Both remaining findings are RUSTSEC-2023-0071 for rsa 0.9.10.
These are not clean vulnerability audits.

## RSA risk and reachability

RustSec lists no patched RSA release. The advisory covers timing leakage during
private-key operations. It still applies to rsa 0.9.10.
Do not classify signing as safe merely because the application does not decrypt RSA ciphertexts.

The main crate directly enables rsa. `src/store_billing/crypto.rs` exports
`sign_rsa_sha256_base64` and `verify_rsa_sha256_base64`.
The signing helper uses `RsaPrivateKey::sign` with PKCS#1 v1.5 SHA-256.
Source inspection finds callers only in `tests/store_payment_crypto.rs`.
No production handler or adapter invokes either RSA helper.
The implemented payment adapters are Epay and Stripe.
Epay signs with its documented MD5 scheme; Stripe callback verification uses HMAC.
This inspection finds no current HTTP route that exposes the RSA helper's private-key timing.
The crate remains enabled and the exported helper remains unsuitable for a new
network-observable signing path without a separate cryptographic replacement review.

The rehearsal lockfile includes rsa through SQLx's optional MySQL dependency.
`cargo tree --locked --manifest-path rehearsal/Cargo.toml --target all -i rsa -e normal,build`
reports no enabled dependency path.
The rehearsal manifest enables SQLite and PostgreSQL, and not MySQL.
Lockfile presence therefore does not establish execution in this rehearsal binary.

## Remaining maintenance warnings

The main audit also reports unmaintained core2 0.4.0, paste 1.0.15, and
proc-macro-error2 2.0.1. All published core2 releases are yanked.
No compatible maintained core2 release exists in the audited registry.
These maintenance notices are distinct from the remaining RSA vulnerability.

The enabled core2 dependency path is:
`monoize -> image 0.25.9 -> ravif 0.12.0 -> rav1e 0.8.1 -> bitstream-io 4.9.0 -> core2 0.4.0`.
Cargo feature inspection confirms `bitstream-io/std` and `core2/std`.
In this configuration, bitstream-io imports `std::io`; core2 re-exports `std::io`
and `std::error`. Its alternate no-std I/O implementations are not enabled.
The image compression transform emits JPEG, PNG, WebP, or JPEG XL and contains no AVIF encoder call.
The AVIF encoder dependency comes from image's default features.
Do not claim the dependency is absent from the build or that an unmaintained package is certified safe.
Review image feature reduction or its encoder dependency upgrades separately.

## Verification limits

The updated lockfiles resolve successfully and were rescanned against the stated RustSec revision.
The portable rustfmt parses the changed authentication, transform, and context files.
Format the three new Rust files; preserve unrelated historical formatting differences.
No local MSVC compiler is installed, so this environment does not establish a Rust build or test pass.
Require the final locked Linux Rust and disposable PostgreSQL verification jobs before deployment.
No live payment, inference, refund, withdrawal, or credential mutation was used for this review.
