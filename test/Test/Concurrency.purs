module Test.Concurrency where

import Prelude
import Control.Parallel (parallel, sequential)
import Data.Array as Array
import Data.Either (Either(..))
import Data.Time.Duration (Milliseconds(..))
import Data.Traversable (traverse, traverse_)
import Effect (Effect, forE)
import Effect.AVar as EAVar
import Effect.Aff (Aff, delay, launchAff_, makeAff, forkAff, joinFiber, never, supervise)
import Effect.Aff.AVar as AVar
import Effect.Class (liftEffect)
import Effect.Console (log)
import Effect.Ref as Ref
import Test.Assert (assertEqual)
import Test.Stress as Stress

foreign import data Rendezvous :: Type
foreign import rendezvous :: Effect Rendezvous
foreign import arrive :: Rendezvous -> Effect Unit
foreign import threadCount :: Rendezvous -> Effect Int
foreign import enqueue :: Effect Unit -> Effect Unit
foreign import data CallbackGate :: Type
foreign import callbackGate :: Effect CallbackGate
foreign import enqueueGated :: CallbackGate -> Effect Unit -> Effect Unit
foreign import releaseCallbacks :: CallbackGate -> Effect Unit

-- An external callback may complete from any runtime worker.
workerYield :: Aff Unit
workerYield = makeAff \done -> do
  enqueue (done (Right unit))
  pure mempty

-- Signals its registration from inside the `makeAff` effect, before suspending.
-- The driver waits on that signal before releasing callbacks: a fork no longer
-- runs its first instructions on the caller's stack.
workerYieldGated :: CallbackGate -> AVar.AVar Unit -> Aff Unit
workerYieldGated gate registered = makeAff \done -> do
  enqueueGated gate (done (Right unit))
  void $ EAVar.tryPut unit registered
  pure mempty

-- The first arrival waits for the second before returning. Distinct thread IDs
-- alone would not prove that the resumptions can execute concurrently.
--
-- `synchronize` must wait for both visits to start if it depends on that, since
-- a fork no longer runs its first instructions on the caller's stack.
assertConcurrentResume
  :: String
  -> (AVar.AVar Unit -> Aff Unit)
  -> (AVar.AVar Unit -> AVar.AVar Unit -> Aff Unit)
  -> Aff Unit
assertConcurrentResume label visit synchronize = do
  meeting <- liftEffect rendezvous
  firstRegistered <- AVar.empty
  secondRegistered <- AVar.empty
  let visitThrough registered = visit registered *> liftEffect (arrive meeting)
  first <- forkAff (visitThrough firstRegistered)
  second <- forkAff (visitThrough secondRegistered)
  synchronize firstRegistered secondRegistered
  joinFiber first
  joinFiber second
  count <- liftEffect $ threadCount meeting
  liftEffect do
    assertEqual { actual: count, expected: 2 }
    log label

-- Two branches of one ParAff must be able to run their synchronous sections at
-- the same time. `arrive` blocks one worker until a second one arrives, so a
-- serialized start can never satisfy it.
assertParallelResume :: Aff Unit
assertParallelResume = do
  meeting <- liftEffect rendezvous
  { r1, r2 } <- sequential $
    { r1: _, r2: _ }
      <$> parallel (liftEffect (arrive meeting) $> "a")
      <*> parallel (liftEffect (arrive meeting) $> "b")
  liftEffect do
    assertEqual { actual: { r1, r2 }, expected: { r1: "a", r2: "b" } }
    log "[OK] parallel branches run synchronous sections concurrently"

main :: Effect Unit
main = launchAff_ do
  gate <- liftEffect callbackGate
  assertConcurrentResume "[OK] Aff resumes on distinct Tokio worker threads"
    (workerYieldGated gate)
    ( \first second -> traverse_ AVar.take [ first, second ]
        *> liftEffect (releaseCallbacks gate)
    )
  assertConcurrentResume "[OK] delay 0 resumptions execute concurrently"
    (const (delay (Milliseconds 0.0)))
    (const <<< const $ pure unit)
  assertConcurrentResume "[OK] positive-delay resumptions execute concurrently"
    (const (delay (Milliseconds 10.0)))
    (const <<< const $ pure unit)

  assertParallelResume

  ref <- liftEffect $ Ref.new 0
  workers <- traverse forkAff $ Array.replicate 8 do
    workerYield
    liftEffect $ forE 0 1000 \_ -> Ref.modify_ (_ + 1) ref
  traverse_ joinFiber workers
  total <- liftEffect $ Ref.read ref
  liftEffect do
    assertEqual { actual: total, expected: 8000 }
    log "[OK] Aff shared Ref: 8000 atomic modifications"

  Stress.stressAVar
  liftEffect $ log "[OK] original Go AVar stress: 1000 items"

  queue <- AVar.empty
  consumed <- liftEffect $ Ref.new 0
  consumers <- traverse forkAff $ Array.replicate 1000 do
    workerYield
    value <- AVar.take queue
    liftEffect $ Ref.modify_ (_ + value) consumed
  producers <- traverse forkAff $ Array.replicate 1000 do
    workerYield
    AVar.put 1 queue
  traverse_ joinFiber consumers
  traverse_ joinFiber producers
  sum <- liftEffect $ Ref.read consumed
  liftEffect do
    assertEqual { actual: sum, expected: 1000 }
    log "[OK] Aff/AVar: 1000 producers and consumers across suspensions"

  supervise do
    void $ forkAff (never :: Aff Unit)
    pure unit
  liftEffect $ log "[OK] supervise cancels an unreferenced never fiber"
