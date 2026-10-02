# Dependency Maintenance Specification

DM1. Dependency changes MUST use the package manager's update, add, or remove commands.
Commit the resulting manifest and lockfile changes together.

DM2. A dependency audit MUST record the advisory database revision, package version,
patched version constraint, and affected manifest. Do not report a clean audit when
an unresolved vulnerability remains in a lockfile.

DM3. Prefer compatible patched releases. A dependency with no source references MAY
be removed after checking implementation, test, and build-script references.
Removing an unused package MUST NOT remove an implemented product feature.

DM4. An advisory affecting an optional lockfile package MUST distinguish lockfile
presence from enabled dependency-graph presence. Record the `cargo tree` feature
evidence used for this distinction. Do not edit a lockfile to conceal a package.

DM5. If no patched release exists, document the affected operations and their
reachable production callers. Do not infer absence of risk from test success or
from the absence of one particular operation such as decryption.

DM6. Updated Rust dependencies MUST pass the project's locked Rust verification
jobs before production deployment. An audit scan alone does not establish runtime
compatibility. Diagnostic audits MUST NOT call live payment or inference providers.
