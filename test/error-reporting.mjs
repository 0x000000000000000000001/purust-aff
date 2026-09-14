// Fresh TAST and child processes are required: a caught Rust test panic would
// not establish stderr or process-exit behavior at the program boundary.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { closeSync, existsSync, globSync, mkdirSync, mkdtempSync, openSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { spawnSync } from 'node:child_process';

const tests = dirname(fileURLToPath(import.meta.url));
const root = resolve(tests, '..');
const compiler = resolve(root, '../purust');
const packages = JSON.parse(readFileSync(join(compiler, 'spago.lock'))).packages;
const native = new Set(['aff', 'arrays', 'avar', 'console', 'effect', 'enums', 'exceptions',
  'foldable-traversable', 'integers', 'lazy', 'numbers', 'partial', 'prelude', 'refs',
  'st', 'strings', 'unfoldable', 'unsafe-coerce']);
const names = new Set();
function visitPackage(name) {
  if (names.has(name)) return;
  assert.ok(packages[name], name);
  names.add(name);
  for (const dependency of packages[name].dependencies) visitPackage(dependency);
}
visitPackage('aff'); visitPackage('console');
const roots = [...names].map(name => native.has(name) ? resolve(root, `../purust-${name}/src`)
  : join(compiler, `.spago/p/${name}-${packages[name].version}/src`));
for (const sourceRoot of roots) assert.ok(existsSync(sourceRoot), sourceRoot);
const fork = resolve(compiler, '../../purescript/.stack-work/dist');
const candidates = globSync('**/build/purs/purs', { cwd: fork });
const purs = process.env.PURS ?? (assert.equal(candidates.length, 1), join(fork, candidates[0]));
const directory = mkdtempSync(join(process.env.PURUST_AFF_ERRORS_KEEP_OUTPUT ?? tmpdir(), 'purust-aff-errors-'));
const commands = [];
const sha256 = file => createHash('sha256').update(readFileSync(file)).digest('hex');
const report = { directory, purs, pursSha256: sha256(purs), bundleSha256: sha256(join(compiler, 'bin/purust.js')), cases: [] };
const save = () => writeFileSync(join(directory, 'report.json'), JSON.stringify(report, null, 2) + '\n');
console.log(`Fresh Aff error-reporting diagnostic: ${directory}`);
function run(command, args, options = {}) {
  const start = Date.now();
  const result = spawnSync(command, args, { cwd: directory, encoding: 'utf8', timeout: 60000,
    maxBuffer: 16 * 1024 * 1024, env: { ...process.env, RUST_BACKTRACE: '0' }, ...options });
  commands.push({ command, args, status: result.status, signal: result.signal, error: result.error?.message,
    elapsedMs: Date.now() - start, stdout: result.stdout, stderr: result.stderr });
  writeFileSync(join(directory, 'commands.json'), JSON.stringify(commands, null, 2) + '\n');
  assert.ok(!result.error && !result.signal, result.error?.message ?? result.signal);
  return result;
}
function checked(command, args) {
  console.log(command + ' ' + args.slice(0, 2).join(' '));
  const result = run(command, args);
  assert.equal(result.status, 0, result.stderr);
  return result.stdout;
}
try {
  report.pursVersion = checked(purs, ['--version']).trim();
  const fixture = join(tests, 'Test/ErrorReporting.purs');
  const graph = JSON.parse(checked(purs, ['graph', fixture,
    ...roots.flatMap(sourceRoot => globSync('**/*.purs', { cwd: sourceRoot }).map(file => join(sourceRoot, file)))]));
  const selected = new Map();
  function visit(name) {
    if (name === 'Prim' || name.startsWith('Prim.') || selected.has(name)) return;
    assert.ok(graph[name], name);
    selected.set(name, resolve(directory, graph[name].path));
    for (const dependency of graph[name].depends) visit(dependency);
  }
  visit('Test.ErrorReporting');
  const tast = join(directory, 'tast');
  checked(purs, ['compile', ...selected.values(), '--codegen', 'corefn', '--output', tast]);
  const input = JSON.parse(readFileSync(join(tast, 'Test.ErrorReporting/corefn.json')));
  assert.ok(Array.isArray(input.typeTable) && Array.isArray(input.dataDecls));
  report.sources = [...selected].map(([module, file]) => ({ module, file, sha256: sha256(file) }));
  report.ffi = [join(root, 'src/Effect/Aff.rs'), resolve(root, '../purust-exceptions/src/Effect/Exception.rs'),
    join(tests, 'Test/ErrorReporting.rs')].map(file => ({ file, sha256: sha256(file) }));
  const rust = join(directory, 'rust');
  checked(process.execPath, ['--stack-size=65536', join(compiler, 'bin/purust.js'),
    '--source', tast, '--out', rust, '--main', 'Test.ErrorReporting', '--threaded']);
  const { threadedRust } = await import(pathToFileURL(join(compiler, 'src/Purust/Threading.js')));
  assert.ok(readFileSync(join(rust, 'Purs_Effect_Aff/src/lib.rs'), 'utf8')
    .includes(threadedRust(readFileSync(join(root, 'src/Effect/Aff.rs'), 'utf8'))));
  const examples = join(rust, 'Purs_Test_ErrorReporting/examples');
  mkdirSync(examples);
  writeFileSync(join(examples, 'errors.rs'), readFileSync(join(tests, 'error-reporting-driver.rs')));
  checked('cargo', ['build', '--offline', '--manifest-path', join(rust, 'Cargo.toml'), '-p', 'Purs_Test_ErrorReporting', '--example', 'errors']);
  const binary = join(rust, 'target/debug/examples/errors');
  report.binarySha256 = sha256(binary);
  const scenarios = [
    { id: 0, status: 0, stdout: 'success\n', stderr: '' },
    { id: 1, status: 0, stdout: 'handled Aff\n', stderr: '' },
    { id: 2, status: 101, stdout: 'finalized Aff\n', stderr: 'ErreurΩ: échec 🚀 漢字\n' },
    { id: 3, status: 101, stdout: 'finalized Effect\n', stderr: 'ErreurEffet: effet 🌍\n' },
    { id: 4, status: 101, stdout: 'child finished\n', stderr: 'Error: synchronous main failure\n' },
    { id: 5, status: 101, stdout: '', panic: true },
    { id: 6, status: 101, stdout: '', panic: true },
    { id: 7, status: 0, stdout: 'handled Effect\n', stderr: '' },
  ];
  for (const scenario of scenarios) {
    const result = run(binary, [String(scenario.id)], { timeout: 15000 });
    report.cases.push({ scenario: scenario.id, status: result.status, stdout: result.stdout, stderr: result.stderr });
    save();
    assert.equal(result.status, scenario.status, result.stderr);
    assert.equal(result.stdout, scenario.stdout);
    if (scenario.panic) {
      assert.equal(result.stderr.split('NATIVE_AFF_PANIC_SENTINEL').length - 1, 1);
      assert.ok(result.stderr.includes('panicked at') && !result.stderr.includes('Error:'));
    } else assert.equal(result.stderr, scenario.stderr, `scenario ${scenario.id}: final error must be reported once, after cleanup`);
    console.log(`scenario ${scenario.id}: passed`);
  }
  const readOnly = openSync(join(tests, 'error-reporting-driver.rs'), 'r');
  try {
    const result = run(binary, ['2', 'catch-outer'], { timeout: 15000, stdio: ['ignore', 'pipe', readOnly] });
    assert.equal(result.status, 0);
    assert.equal(result.stdout, 'finalized Aff\noriginal error preserved\n');
    report.cases.push({ scenario: 'unwritable-stderr', status: result.status, stdout: result.stdout });
  } finally { closeSync(readOnly); }
  for (const { file, sha256: hash } of [...report.sources, ...report.ffi]) assert.equal(sha256(file), hash, file);
  report.passed = true;
  console.log(`${selected.size} fresh modules; 9 error-reporting scenarios passed.`);
} finally { save(); }
