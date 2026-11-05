#!/usr/bin/env bash
# Enforces the hexagonal dependency direction (ADR-006) on the Cargo workspace:
#
#   src-tauri -> infra-* -> application -> domain
#
#   tan-domain        depends on no workspace crate and on no external crate
#   tan-application   may depend on tan-domain only (among workspace crates)
#   tan-infra-*       may depend on tan-application and tan-domain only;
#                     infra crates must not depend on each other
#   tan-app           (src-tauri, composition root) may depend on anything
#
# Only normal and build dependencies are checked; dev-dependencies are ignored.
# Requires: cargo, node (used to parse `cargo metadata` JSON).
set -euo pipefail

cd "$(dirname "$0")/.."

# External crates tan-domain is explicitly allowed to use (space separated).
# Keep this empty unless an ADR says otherwise.
DOMAIN_ALLOWED_EXTERNAL="${DOMAIN_ALLOWED_EXTERNAL:-}"

cargo metadata --format-version 1 --no-deps | DOMAIN_ALLOWED_EXTERNAL="$DOMAIN_ALLOWED_EXTERNAL" node -e '
const fs = require("node:fs");
const meta = JSON.parse(fs.readFileSync(0, "utf8"));
const allowedExternal = new Set((process.env.DOMAIN_ALLOWED_EXTERNAL || "").split(/\s+/).filter(Boolean));

const members = new Set(meta.packages.map((p) => p.name));
const errors = [];

for (const pkg of meta.packages) {
  const deps = pkg.dependencies
    .filter((d) => d.kind === null || d.kind === "build")
    .map((d) => d.name);
  const workspaceDeps = deps.filter((d) => members.has(d));
  const externalDeps = deps.filter((d) => !members.has(d));

  let allowedWorkspace;
  if (pkg.name === "tan-domain") {
    allowedWorkspace = [];
    for (const dep of externalDeps) {
      if (!allowedExternal.has(dep)) {
        errors.push(`tan-domain must have no external dependencies, found: ${dep}`);
      }
    }
  } else if (pkg.name === "tan-application") {
    allowedWorkspace = ["tan-domain"];
  } else if (pkg.name.startsWith("tan-infra-")) {
    allowedWorkspace = ["tan-application", "tan-domain"];
  } else if (pkg.name === "tan-app") {
    allowedWorkspace = [...members];
  } else {
    errors.push(`unclassified workspace crate "${pkg.name}": add it to scripts/check-deps.sh`);
    continue;
  }

  for (const dep of workspaceDeps) {
    if (!allowedWorkspace.includes(dep)) {
      errors.push(`${pkg.name} must not depend on ${dep}`);
    }
  }
}

if (errors.length > 0) {
  console.error("Dependency direction violations:");
  for (const e of errors) console.error(`  - ${e}`);
  process.exit(1);
}
console.log(`check-deps: OK (${members.size} workspace crates)`);
'
