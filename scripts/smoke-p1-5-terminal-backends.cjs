#!/usr/bin/env node
// P1-5 smoke: verify the multi-backend terminal abstraction is wired.
// Static smoke — it asserts the TerminalBackend trait, LocalBackend,
// SSH backend (feature-gated), and the new DockerBackend all exist
// with the correct shape, plus BackendManager routes by enum variant.

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
let failed = 0;
function check(name, cond, hint) {
  const status = cond ? 'OK ' : 'FAIL';
  console.log(`[${status}] ${name}${hint ? '  — ' + hint : ''}`);
  if (!cond) failed++;
}

// 1. TerminalBackend trait lives in hermix-core.
const traits = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-core/src/traits.rs'),
  'utf8'
);
check(
  'TerminalBackend trait defined in hermix-core',
  /pub trait TerminalBackend: Send \+ Sync/.test(traits) &&
    /async fn execute_command/.test(traits) &&
    /async fn read_file/.test(traits) &&
    /async fn write_file/.test(traits) &&
    /async fn file_exists/.test(traits),
  '4 methods: execute_command, read_file, write_file, file_exists'
);

// 2. TerminalBackendType enum covers all variants in config.
const config = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-config/src/config.rs'),
  'utf8'
);
check(
  'TerminalBackendType enum has Local + Docker + Ssh',
  /pub enum TerminalBackendType/.test(config) &&
    /Local,/.test(config) &&
    /Docker,/.test(config) &&
    /Ssh,/.test(config),
  'enum variants (Local default; Docker + Ssh opt-in)'
);
check(
  'TerminalConfig has docker_container field',
  /pub docker_container: String/.test(config) &&
    /default_docker_container/.test(config),
  'P1-5: configurable container name'
);

// 3. hermix-environments crate wires Local + SSH + Docker backends.
const envLib = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-environments/src/lib.rs'),
  'utf8'
);
check(
  'hermix-environments declares docker module feature-gated',
  /#\[cfg\(feature = "docker"\)\][\s\S]{0,40}pub mod docker/.test(envLib),
  'opt-in to avoid paying the compile cost for users who never sandbox'
);
check(
  'hermix-environments re-exports LocalBackend',
  /pub use local::LocalBackend/.test(envLib),
  'default backend'
);

const mgr = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-environments/src/manager.rs'),
  'utf8'
);
check(
  'BackendManager.create_backend routes Local',
  /TerminalBackendType::Local\s*=>\s*[\s\S]{0,80}LocalBackend::new/.test(mgr),
  'local backend wired'
);
check(
  'BackendManager.create_backend routes Docker when feature enabled',
  /#\[cfg\(feature = "docker"\)\][\s\S]{0,200}TerminalBackendType::Docker\s*=>\s*[\s\S]{0,80}DockerBackend::new/.test(mgr),
  'docker backend wired'
);
check(
  'BackendManager.create_backend routes SSH when feature enabled',
  /#\[cfg\(feature = "ssh"\)\][\s\S]{0,200}TerminalBackendType::Ssh\s*=>\s*[\s\S]{0,80}SshBackend::new/.test(mgr),
  'ssh backend wired'
);
check(
  'BackendManager.switch_backend validates feature availability',
  /switch_backend/.test(mgr) && /feature not enabled/.test(mgr),
  'runtime fallback surfaces a clear error'
);

// 4. LocalBackend implements the trait with sane defaults.
const local = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-environments/src/local.rs'),
  'utf8'
);
check(
  'LocalBackend has execute_command via tokio::process',
  /impl TerminalBackend for LocalBackend/.test(local) &&
    /tokio::process::Command/.test(local),
  'local execution uses tokio async process'
);
check(
  'LocalBackend has unit tests',
  /#\[cfg\(test\)\]/.test(local) &&
    /fn test_execute_command_echo/.test(local),
  'baseline correctness covered'
);

// 5. DockerBackend implementation shape (always present, even without
//    the feature enabled in this smoke — we read the file directly).
const docker = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-environments/src/docker.rs'),
  'utf8'
);
check(
  'DockerBackend struct declared',
  /pub struct DockerBackend/.test(docker) &&
    /container: String/.test(docker) &&
    /run_as_root: bool/.test(docker),
  'struct + fields'
);
check(
  'DockerBackend implements TerminalBackend',
  /impl TerminalBackend for DockerBackend/.test(docker),
  'trait impl'
);
check(
  'DockerBackend executes via docker exec',
  /TokioCommand::new\("docker"\)/.test(docker) &&
    /build_exec_prefix/.test(docker) &&
    /"exec"/.test(docker),
  'CLI invocation (no bollard dep)'
);
check(
  'DockerBackend escapes shell arguments',
  /fn shell_escape/.test(docker) && /'\\\\''/.test(docker) || /shell_escape.*single quote/i.test(docker),
  'LLM-supplied paths cannot inject shell commands'
);
check(
  'DockerBackend has unit tests',
  /#\[cfg\(test\)\]/.test(docker) &&
    /fn build_exec_prefix_default_is_unprivileged/.test(docker),
  'unit tests for prefix + escape + truncation'
);
check(
  'DockerBackend truncates output at max_output_size',
  /fn make_output/.test(docker) && /\.truncate\(cap\)/.test(docker),
  'memory-bounded output (matches LocalBackend)'
);

// 6. Workspace has docker feature in hermix-environments Cargo.toml.
const envCargo = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-environments/Cargo.toml'),
  'utf8'
);
check(
  'docker feature declared in Cargo.toml',
  /\[features\][\s\S]{0,200}docker = \[\]/.test(envCargo),
  'feature flag exists and is empty (zero compile-time cost)'
);

console.log(failed === 0 ? '\nALL OK' : `\n${failed} FAILED`);
process.exit(failed === 0 ? 0 : 1);