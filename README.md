# Aff

[![CI](https://github.com/purescript-contrib/purescript-aff/workflows/CI/badge.svg?branch=main)](https://github.com/purescript-contrib/purescript-aff/actions?query=workflow%3ACI+branch%3Amain)
[![Release](https://img.shields.io/github/release/purescript-contrib/purescript-aff.svg)](https://github.com/purescript-contrib/purescript-aff/releases)
[![Pursuit](https://pursuit.purescript.org/packages/purescript-aff/badge)](https://pursuit.purescript.org/packages/purescript-aff)
[![Maintainer: natefaubion](https://img.shields.io/badge/maintainer-natefaubion-teal.svg)](https://github.com/natefaubion)

An asynchronous effect monad and threading model for PureScript.

## Installation

Install `aff` with [Spago](https://github.com/purescript/spago):

```sh
spago install aff
```

## Quick start

This quick start covers common, minimal use cases for the library. Longer examples and tutorials can be found in the [docs directory](./docs).

```purescript
main :: Effect Unit
main = launchAff_ do
  response <- Ajax.get "http://foo.bar"
  log response.body
```

## Documentation

`aff` documentation is stored in a few places:

1. Module documentation is [published on Pursuit](https://pursuit.purescript.org/packages/purescript-aff).
2. Written documentation is kept in the [docs directory](./docs).
3. Usage examples can be found in [the test suite](./test).

If you get stuck, there are several ways to get help:

- [Open an issue](https://github.com/purescript-contrib/purescript-aff/issues) if you have encountered a bug or problem.
- Ask general questions on the [PureScript Discourse](https://discourse.purescript.org) forum or the [PureScript Discord](https://purescript.org/chat) chat.

## Contributing

You can contribute to `aff` in several ways:

1. If you encounter a problem or have a question, please [open an issue](https://github.com/purescript-contrib/purescript-aff/issues). We'll do our best to work with you to resolve or answer it.

2. If you would like to contribute code, tests, or documentation, please [read the contributor guide](./CONTRIBUTING.md). It's a short, helpful introduction to contributing to this library, including development instructions.

3. If you have written a library, tutorial, guide, or other resource based on this package, please share it on the [PureScript Discourse](https://discourse.purescript.org)! Writing libraries and learning resources are a great way to help this library succeed.

## Rust backend

Use the sibling Purust compiler with `--threaded`. Generated values use atomic
shared ownership and thread-safe callbacks; the Aff interpreter uses Tokio for
waiting and asynchronous resumptions.

Fiber startup is free. `forkAff`, `launchAff`/`Fiber.run`, `joinFiber` and every
`ParAff` leaf are submitted to a bounded CPU pool instead of running on the
caller's stack, so parallel branches and independent fibers can run their
synchronous sections concurrently. Set `PURUST_AFF_WORKERS` to choose the pool
size; the default is `max(2, available_parallelism())`. No start, execution or
completion order is guaranteed between independent fibers, so two synchronous
`parTraverse` branches may genuinely overlap. `parTraverse` still returns its
results in index order, and a fiber still runs its own instructions in order
until its first suspension. `suspendAff` remains suspended until observed.

Cancellation is considered between instructions of the target fiber. A fiber
killed before it started never runs its body, so a `bracket` only ever runs its
finalizer for a resource it acquired; a synchronous section that has begun is
never interrupted from the outside, and the cancellation request is observed at
the next suspension point. Killing a fiber that already completed is a no-op.

Expired timers are dispatched in deadline/registration order to independent
Tokio blocking tasks, so synchronous CPU work after a delay can run in parallel
without blocking the timer loop or the async workers. A delay always suspends,
even when its callback arrives during registration. Foreign callbacks can
complete on different workers.

The generated entry point keeps the runtime alive until all active fibers finish,
including children and grandchildren that outlive their parents, and until every
submitted start has run. Unhandled Aff errors produce a failing process after the
remaining children finish. A Rust panic is fatal without waiting for suspended
children, including `never` fibers. `supervise` retains its own cancellation
semantics.

Run the Rust tests with:

```sh
./bin/test -c
```

This rebuilds Purust using its local Spago, regenerates TAST with the sibling
PureScript fork, compiles Rust and checks the output. Set `PURS` to select another
TAST-enabled compiler. `./bin/test --smoke` runs the small initial scenario.

The full runner preserves all 47 active tests in `test/Test/Main.purs`, including
cancellation, bracket, supervision, parallel races, recursion, the 100,000-item
stack test, the 100,000-fork scheduler test and a `parTraverse` check that keeps
result order while later indices complete first. It also runs the unchanged Go
AVar stress test, real worker-thread Ref/AVar integration, overlapping
resumptions after zero and positive delays, a rendezvous proving that two
parallel branches overlap inside their synchronous sections, supervision of an
unreferenced `never` fiber, and parent/child lifetime scenarios. Parallel checks
use joins and gates to synchronize branches, without assuming a start or
completion order between independent timers. Rust unit tests prove that two
synchronous parallel leaves overlap on the pool, that a single worker serializes
them and that nested parallelism never deadlocks, and they force an early
callback to verify the delay handoff.

To compare pool sizes on CPU-bound `parTraverse` work, build the benchmark
(`--main Test.ParBench`) and run it with different `PURUST_AFF_WORKERS` values
under `/usr/bin/time -l`; see `todo.md` for the recorded measurements.

The Rust runtime does not collect strong reference cycles automatically. As with
other `Rc`/`Arc` values, a strong self-reference must be broken to release it.

The Rust runtime does not collect strong reference cycles automatically. As with
other `Rc`/`Arc` values, a strong self-reference must be broken to release it.
