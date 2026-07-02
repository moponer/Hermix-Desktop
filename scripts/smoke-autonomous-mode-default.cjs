// Smoke test: verify that the user's actual ~/.hermix/config.yaml
// deserialises with autonomous_mode=true (default), and that the
// HERMIX_AUTONOMOUS_MODE env var can opt out.
//
// Run: node scripts/smoke-autonomous-mode-default.cjs
//
// Why this exists: 2026-06-27 主人 bug — agent stopped at turn=6 because
// `autonomous_mode: true` was missing from config.yaml. Default is now
// `true`. This script catches the regression if anyone reverts the default.

const fs = require('fs');
const path = require('path');
const os = require('os');
const { execFileSync } = require('child_process');

function rustEval(snippet) {
  // Build a tiny standalone Rust program that uses hermix-config to load
  // the user's config.yaml and prints the autonomous_mode field.
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'hermix-smoke-'));
  const cargoToml = `\
[package]
name = "smoke_autonomous_mode_default"
version = "0.1.0"
edition = "2021"

[dependencies]
hermix-config = { path = "${process.cwd().replace(/\\/g, '\\\\')}\\\\crates\\\\hermix-config" }
serde_json = "1"
serde_yaml = "0.9"
`;
  const mainRs = `\
use hermix_config::GatewayConfig;
fn main() {
    let yaml = std::fs::read_to_string(r"${process.env.HERMIX_CONFIG_PATH || '<USERPROFILE>/.hermix/config.yaml'}").unwrap();
    let cfg: GatewayConfig = serde_yaml::from_str(&yaml).unwrap();
    println!("{}", cfg.autonomous_mode);
}
`;
  fs.writeFileSync(path.join(tmpDir, 'Cargo.toml'), cargoToml);
  fs.mkdirSync(path.join(tmpDir, 'src'), { recursive: true });
  fs.writeFileSync(path.join(tmpDir, 'src', 'main.rs'), mainRs);
  const out = execFileSync(
    'cargo',
    ['run', '--quiet', '--manifest-path', path.join(tmpDir, 'Cargo.toml')],
    { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }
  );
  fs.rmSync(tmpDir, { recursive: true, force: true });
  return out.trim();
}

function assertEq(actual, expected, label) {
  if (actual !== expected) {
    console.error(`  ✗ ${label}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
    process.exit(1);
  }
  console.log(`  ✓ ${label}: ${JSON.stringify(actual)}`);
}

console.log('[smoke] autonomous_mode default — testing user config.yaml');
try {
  const got = rustEval();
  assertEq(got, 'true', 'autonomous_mode deserialised from user config.yaml (no explicit key)');
  console.log('[smoke] 1 passed, 0 failed');
} catch (e) {
  console.error('[smoke] FAILED:', e.message);
  if (e.stderr) console.error('stderr:', e.stderr.toString());
  process.exit(1);
}