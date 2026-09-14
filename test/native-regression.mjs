// Isolated equivalent of the native suites in bin/test; never clears output or caches.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, globSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
const tests = dirname(fileURLToPath(import.meta.url)), root = resolve(tests, '..'), compiler = resolve(root, '../purust');
const packages = JSON.parse(readFileSync(join(compiler, 'spago.lock'))).packages;
const names = new Set();
function visitPackage(name) {
  if (names.has(name)) return;
  assert.ok(packages[name], name); names.add(name); packages[name].dependencies.forEach(visitPackage);
}
['aff', 'arrays', 'console', 'refs'].forEach(visitPackage);
const roots = [...names].map(name => {
  const native = resolve(root, `../purust-${name}/src`);
  return existsSync(native) ? native : join(compiler, `.spago/p/${name}-${packages[name].version}/src`);
});
roots.push(tests, resolve(root, '../purust-assert/src'), resolve(root, '../purust-avar/src'));
const index = new Map();
for (const path of roots.flatMap(path => globSync('**/*.purs', { cwd: path }).map(file => join(path, file)))) {
  const source = readFileSync(path, 'utf8'), name = source.match(/^module\s+([A-Z][\w.]*)\s/m)?.[1];
  assert.ok(name && !index.has(name), path);
  index.set(name, { path, depends: [...source.matchAll(/^import\s+([A-Z][\w.]*)\b/gm)].map(m => m[1]) });
}
const selected = new Map();
function select(name) {
  if (name === 'Prim' || name.startsWith('Prim.') || selected.has(name)) return;
  assert.ok(index.has(name), name); selected.set(name, index.get(name).path); index.get(name).depends.forEach(select);
}
const suites = [['Test.Main', 'main'], ['Test.Concurrency', 'concurrency'], ['Test.Lifetime', 'lifetime']];
suites.forEach(([name]) => select(name));
const fork = resolve(compiler, '../../purescript/.stack-work/dist');
const candidates = globSync('**/build/purs/purs', { cwd: fork });
const purs = process.env.PURS ?? (assert.equal(candidates.length, 1), join(fork, candidates[0]));
const directory = mkdtempSync(join(process.env.PURUST_AFF_REGRESSION_OUTPUT ?? tmpdir(), 'purust-aff-regression-'));
const hash = path => createHash('sha256').update(readFileSync(path)).digest('hex');
const report = { complete: false, directory, inputs: [join(compiler, 'bin/purust.js'), ...selected.values(),
  ...[...selected.values()].map(path => path.replace(/\.purs$/, '.rs')).filter(existsSync)]
  .map(path => ({ path, sha256: hash(path) })), commands: [] };
const save = () => writeFileSync(join(directory, 'report.json'), JSON.stringify(report, null, 2) + '\n');
function run(label, command, args, expected = 0) {
  console.log(label);
  const start = Date.now();
  const result = spawnSync(command, args, { cwd: directory, encoding: 'utf8', timeout: 180000, maxBuffer: 32 * 1024 * 1024,
    env: { ...process.env, GHCRTS: '-N2', CARGO_BUILD_JOBS: '1', CARGO_PROFILE_DEV_DEBUG: '0', CARGO_INCREMENTAL: '0' } });
  writeFileSync(join(directory, label + '.json'), JSON.stringify(result, null, 2) + '\n');
  report.commands.push({ label, command, args, status: result.status, signal: result.signal, elapsedMs: Date.now() - start }); save();
  assert.equal(result.status, expected, `${result.error ?? ''}\n${result.stderr.slice(-6000)}\n${result.stdout.slice(-3000)}`);
  return result;
}
console.log(`Fresh Aff regression: ${directory}`);
try {
  run('graph', purs, ['graph', ...selected.values()]);
  const tast = join(directory, 'tast'), rust = join(directory, 'rust');
  run('tast', purs, ['compile', ...selected.values(), '--codegen', 'corefn', '--output', tast]);
  assert.ok(Array.isArray(JSON.parse(readFileSync(join(tast, 'Effect.Aff/corefn.json'))).typeTable));
  for (const [main, label] of suites) {
    run('generate-' + label, process.execPath, ['--stack-size=65536', join(compiler, 'bin/purust.js'), '--source', tast, '--out', rust, '--main', main, '--threaded']);
    run('build-' + label, 'cargo', ['build', '--offline', '--manifest-path', join(rust, 'Cargo.toml'), '-p', 'purust_output']);
    const binary = join(rust, 'target/debug/purust_output');
    const result = run('execute-' + label, binary, []);
    assert.equal(result.stdout, readFileSync(join(tests, `expected-${label}.stdout`), 'utf8'));
    assert.equal(result.stderr, '');
    if (label === 'lifetime') for (const scenario of [1, 2, 3]) {
      const result = run('lifetime-failure-' + scenario, binary, [String(scenario)], 101);
      assert.equal(result.stdout, readFileSync(join(tests, scenario === 3 ? 'expected-lifetime.stdout' : 'expected-lifetime-failure.stdout'), 'utf8'));
      if (scenario !== 1) assert.ok(result.stderr.includes('intentional lifetime Rust panic'));
    }
  }
  for (const { path, sha256 } of report.inputs) assert.equal(hash(path), sha256, path);
  report.complete = true;
  console.log('45 Aff checks, concurrent Ref/AVar integration and lifetime success/failures passed.');
} finally { save(); }
