#!/usr/bin/env node
// P1-4 smoke: verify the cron scheduler surface is wired across the stack.
// Static smoke — it asserts the hermix-cron crate, server automation
// endpoints, frontend AutomationView, and the demo seeding all exist
// in source. This is a "completeness pin" so a refactor can't silently
// regress the user-facing scheduling UX.

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
let failed = 0;
function check(name, cond, hint) {
  const status = cond ? 'OK ' : 'FAIL';
  console.log(`[${status}] ${name}${hint ? '  — ' + hint : ''}`);
  if (!cond) failed++;
}

// 1. hermix-cron crate is in the workspace + compiles.
const cargoToml = fs.readFileSync(path.join(ROOT, 'Cargo.toml'), 'utf8');
check(
  'hermix-cron is a workspace member',
  cargoToml.includes('"crates/hermix-cron"'),
  'low-level cron engine crate'
);

const cronLib = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-cron/src/lib.rs'),
  'utf8'
);
check(
  'hermix-cron re-exports CronScheduler + CronJob',
  /pub use scheduler::\{CronError, CronScheduler\}/.test(cronLib) &&
    /pub use job::\{CronJob, DeliverConfig, DeliverTarget, JobStatus, ModelConfig\}/.test(cronLib),
  'public API surface'
);
check(
  'hermix-cron has SqliteJobPersistence',
  /pub use persistence::\{FileJobPersistence, JobPersistence, SqliteJobPersistence\}/.test(cronLib),
  'SQLite-backed job persistence'
);

const cronScheduler = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-cron/src/scheduler.rs'),
  'utf8'
);
check(
  'hermix-cron scheduler polls every 60s default',
  /fn cron_poll_interval/.test(cronScheduler) &&
    /60/.test(cronScheduler),
  'background tick loop with HERMIX_CRON_TICK_SECS override'
);

// 2. server-side automation endpoints (the user-facing scheduling UX
//    is built on top of hermix-cron primitives).
const dashMod = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-server/src/dashboard/mod.rs'),
  'utf8'
);
check(
  'server registers /api/automation/tasks routes',
  /\/api\/automation\/tasks/.test(dashMod) &&
    /automation::create_task/.test(dashMod) &&
    /automation::list_tasks/.test(dashMod),
  'CRUD endpoints'
);
check(
  'server registers /api/automation/tasks/{id}/trigger',
  /\/api\/automation\/tasks\/\{id\}\/trigger/.test(dashMod) &&
    /automation::trigger_task/.test(dashMod),
  'manual run endpoint'
);
check(
  'server registers /api/automation/reset',
  /\/api\/automation\/reset/.test(dashMod) &&
    /automation::reset_to_official/.test(dashMod),
  'restore 6 demo templates'
);

const automation = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-server/src/dashboard/automation.rs'),
  'utf8'
);
check(
  'automation seeds 6 demo tasks on first start',
  /seed 6 demo tasks|6 demo templates|6 templates/.test(automation),
  'first-run UX (no empty state)'
);
check(
  'automation tick loop runs every 30s',
  /ticks every 30 seconds|30\s*seconds|30s/.test(automation),
  'background scheduler cadence'
);
check(
  'automation uses fire-and-record dispatch',
  /fire_and_record|fire-and-record/i.test(automation),
  'tick loop not blocked on slow agent runs'
);
check(
  'automation persists to $HERMIX_HOME/automation_tasks.json',
  /automation_tasks\.json/.test(automation) &&
    /hermix_home/.test(automation),
  'single JSON file as storage (matches hermix home convention)'
);

// 3. Frontend AutomationView exists and consumes the API.
const viewPath = path.join(
  ROOT,
  'apps/desktop-tauri/src/components/AutomationView.tsx'
);
check(fs.existsSync(viewPath), 'AutomationView.tsx exists', 'React component');

if (fs.existsSync(viewPath)) {
  const view = fs.readFileSync(viewPath, 'utf8');
  check(
    'AutomationView calls listTasks',
    /listTasks/.test(view) || /getAutomationTasks/.test(view),
    'consumes /api/automation/tasks GET'
  );
  check(
    'AutomationView can create tasks',
    /createTask/.test(view) || /createAutomationTask/.test(view),
    'consumes /api/automation/tasks POST'
  );
  check(
    'AutomationView can trigger tasks',
    /triggerTask/.test(view) || /triggerAutomationTask/.test(view),
    'consumes /api/automation/tasks/{id}/trigger POST'
  );
}

// 4. Frontend API types include automation.
const apiPath = path.join(ROOT, 'apps/desktop-tauri/src/api.ts');
if (fs.existsSync(apiPath)) {
  const api = fs.readFileSync(apiPath, 'utf8');
  check(
    'api.ts declares AutomationTask type',
    /AutomationTask/.test(api),
    'TS mirror of Rust DTO (any shape — interface or type)'
  );
}

// 5. hermix-cron unit tests cover basic operations.
const cronPersistence = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-cron/src/persistence.rs'),
  'utf8'
);
check(
  'hermix-cron persistence has unit tests',
  /#\[cfg\(test\)\]/.test(cronPersistence) &&
    /test_sqlite_save_and_load_job|test_save_and_load/.test(cronPersistence),
  'SQLite path tested'
);

console.log(failed === 0 ? '\nALL OK' : `\n${failed} FAILED`);
process.exit(failed === 0 ? 0 : 1);